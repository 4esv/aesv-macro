//! Turns on iCloud Mail sync for the Macro user the stack's connect script named.

use email_api_client::domain::models::EmailApiError;
use email_api_client::domain::ports::MailboxContentReader;
use email_api_client::outbound::imap::ImapMailboxRepository;
use sqlx::PgPool;
use uuid::Uuid;

use crate::outbound::email_api::ICloudEnvCredential;

struct Saved {
    id: Uuid,
    sync_generation: i64,
    /// True when mail sync starts with this link, rather than was already on.
    started: bool,
}

/// Why a link was not made.
#[derive(Debug, thiserror::Error)]
pub enum ICloudLinkError {
    /// The stack has no iCloud login, or no `ICLOUD` link row names an owner.
    #[error("iCloud Mail is not configured on this stack")]
    NotConfigured,
    /// iCloud refused the login.
    #[error("iCloud refused the Apple ID or app-specific password")]
    Refused,
    /// The link row names another Macro user as the owner.
    #[error("the iCloud account is linked to another user")]
    Taken,
    /// iCloud did not answer, or the link is being removed.
    #[error("iCloud Mail is unavailable")]
    Unavailable,
    /// The link could not be saved.
    #[error("the iCloud link could not be saved")]
    Database(#[from] sqlx::Error),
}

/// Turns on mail sync for the `ICLOUD` link row that
/// `deploy/aesv/connect-icloud.sql` wrote, and seeds the folder catalog stream
/// the durable mailbox sync engine claims to start its first sync.
///
/// NOTE: the connect script is the one step that names the mailbox's owner.
/// This never writes a link row, so no other user can take the stack's mailbox.
#[derive(Clone)]
pub struct ICloudLinks {
    /// MacroDB.
    pub db: PgPool,
    /// The adapter that proves the login before mail sync turns on.
    pub repository: ImapMailboxRepository,
}

impl ICloudLinks {
    /// Whether `macro_id` owns the stack's iCloud link and the stack has its login.
    pub async fn available(&self, macro_id: &str) -> Result<bool, sqlx::Error> {
        let Some(address) = Self::address() else {
            return Ok(false);
        };
        Ok(self.owner(&address).await?.as_deref() == Some(macro_id))
    }

    /// Turn on mail sync for `macro_id`'s iCloud link. Linking again is a no-op.
    pub async fn link(&self, macro_id: &str) -> Result<Uuid, ICloudLinkError> {
        let address = Self::address().ok_or(ICloudLinkError::NotConfigured)?;
        let token = ICloudEnvCredential::token().map_err(|_| ICloudLinkError::NotConfigured)?;
        // Refuse a caller who does not own the row before the stack logs in for it.
        match self.owner(&address).await? {
            None => return Err(ICloudLinkError::NotConfigured),
            Some(owner) if owner != macro_id => return Err(ICloudLinkError::Taken),
            Some(_) => {}
        }
        // A login proves the password before sync starts retrying it.
        self.repository
            .folders(&token)
            .await
            .map_err(|error| match error {
                EmailApiError::AuthRequired => ICloudLinkError::Refused,
                error => {
                    tracing::warn!(%error, "iCloud Mail did not answer the link's login");
                    ICloudLinkError::Unavailable
                }
            })?;
        self.save(&address, macro_id).await
    }

    fn address() -> Option<String> {
        ICloudEnvCredential::apple_id().map(|apple_id| apple_id.trim().to_lowercase())
    }

    async fn owner(&self, address: &str) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar!(
            "SELECT macro_id FROM email_links WHERE provider = 'ICLOUD' AND lower(email_address) = $1 ORDER BY created_at LIMIT 1",
            address
        )
        .fetch_optional(&self.db)
        .await
    }

    async fn save(&self, address: &str, macro_id: &str) -> Result<Uuid, ICloudLinkError> {
        let mut tx = self.db.begin().await?;
        sqlx::query!(
            "SELECT 1 AS locked FROM pg_advisory_xact_lock(hashtextextended($1,0))",
            format!("icloud:{address}")
        )
        .fetch_one(&mut *tx)
        .await?;
        // Checked again under the lock: the row may have changed since `link` read it.
        let existing = sqlx::query!(
            r#"SELECT id,macro_id,is_sync_active,
                (deleting_at IS NOT NULL OR disconnect_requested_at IS NOT NULL) AS "removing!"
            FROM email_links WHERE provider = 'ICLOUD' AND lower(email_address) = $1 FOR UPDATE"#,
            address
        )
        .fetch_all(&mut *tx)
        .await?;
        let row = match existing.as_slice() {
            [] => return Err(ICloudLinkError::NotConfigured),
            rows if rows.iter().any(|row| row.macro_id != macro_id) => {
                return Err(ICloudLinkError::Taken);
            }
            rows if rows.iter().any(|row| row.removing) => {
                return Err(ICloudLinkError::Unavailable);
            }
            [row, ..] => row,
        };
        let generation = sqlx::query_scalar!(
            r#"UPDATE email_links SET is_sync_active = true,needs_reauth = false,
                last_sync_error_at = NULL,updated_at = now()
            WHERE id = $1 RETURNING sync_generation"#,
            row.id
        )
        .fetch_one(&mut *tx)
        .await?;
        let link = Saved {
            id: row.id,
            sync_generation: generation,
            started: !row.is_sync_active,
        };
        sqlx::query!(
            "INSERT INTO email_settings (link_id) VALUES ($1) ON CONFLICT DO NOTHING",
            link.id
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            r#"
            INSERT INTO email_sync_streams (id,link_id,generation,kind,scope_id)
            VALUES ($1,$2,$3,'folder_catalog','mail') ON CONFLICT DO NOTHING
        "#,
            macro_uuid::generate_uuid_v7(),
            link.id,
            link.sync_generation
        )
        .execute(&mut *tx)
        .await?;
        let event =
            serde_json::json!({"kind":"link_connected","actor_id":macro_id,"is_new":link.started});
        sqlx::query!(
            "INSERT INTO email_projection_outbox (id,link_id,generation,payload) VALUES ($1,$2,$3,$4)",
            macro_uuid::generate_uuid_v7(),
            link.id,
            link.sync_generation,
            event
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        if link.started {
            email::domain::mailbox::progress::log_import_started(link.id, link.sync_generation);
        }
        Ok(link.id)
    }
}
