//! Links the stack's iCloud Mail account to a Macro user and starts its sync.

use email_api_client::domain::models::EmailApiError;
use email_api_client::domain::ports::MailboxContentReader;
use email_api_client::outbound::imap::ImapMailboxRepository;
use sqlx::PgPool;
use uuid::Uuid;

use crate::outbound::email_api::ICloudEnvCredential;

#[cfg(test)]
mod test;

struct Saved {
    id: Uuid,
    sync_generation: i64,
    /// True when mail sync starts with this link, rather than was already on.
    started: bool,
}

/// Why a link was not made.
#[derive(Debug, thiserror::Error)]
pub enum ICloudLinkError {
    /// The stack has no Apple ID and app-specific password.
    #[error("iCloud Mail is not configured on this stack")]
    NotConfigured,
    /// iCloud refused the login.
    #[error("iCloud refused the Apple ID or app-specific password")]
    Refused,
    /// Another Macro user linked the account first.
    #[error("the iCloud account is linked to another user")]
    Taken,
    /// iCloud did not answer, or the link is being removed.
    #[error("iCloud Mail is unavailable")]
    Unavailable,
    /// The link could not be saved.
    #[error("the iCloud link could not be saved")]
    Database(#[from] sqlx::Error),
}

/// Writes an `ICLOUD` email link and the folder catalog stream the durable
/// mailbox sync engine claims to start its first sync.
#[derive(Clone)]
pub struct ICloudLinks {
    /// MacroDB.
    pub db: PgPool,
    /// The adapter that proves the login before the link is saved.
    pub repository: ImapMailboxRepository,
}

impl ICloudLinks {
    /// Whether the stack has an iCloud login to link.
    pub fn available() -> bool {
        ICloudEnvCredential::token().is_ok()
    }

    /// Link the account for `macro_id`. Linking again re-enables the link.
    ///
    /// NOTE: the stack holds one iCloud login, so the first user to link it
    /// owns it, and any other user is refused with `Taken`.
    pub async fn link(
        &self,
        macro_id: &str,
        fusionauth_user_id: &str,
    ) -> Result<Uuid, ICloudLinkError> {
        let address = ICloudEnvCredential::apple_id()
            .ok_or(ICloudLinkError::NotConfigured)?
            .trim()
            .to_lowercase();
        let token = ICloudEnvCredential::token().map_err(|_| ICloudLinkError::NotConfigured)?;
        // A login proves the password before a row exists that sync would retry.
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
        self.save(&address, macro_id, fusionauth_user_id).await
    }

    async fn save(
        &self,
        address: &str,
        macro_id: &str,
        fusionauth_user_id: &str,
    ) -> Result<Uuid, ICloudLinkError> {
        let mut tx = self.db.begin().await?;
        sqlx::query!(
            "SELECT 1 AS locked FROM pg_advisory_xact_lock(hashtextextended($1,0))",
            format!("icloud:{address}")
        )
        .fetch_one(&mut *tx)
        .await?;
        // The calendar's connect script may have written the link with mail
        // sync off; linking mail turns that row on rather than adding another.
        let existing = sqlx::query!(
            r#"SELECT id,macro_id,is_sync_active,
                (deleting_at IS NOT NULL OR disconnect_requested_at IS NOT NULL) AS "removing!"
            FROM email_links WHERE provider = 'ICLOUD' AND lower(email_address) = $1 FOR UPDATE"#,
            address
        )
        .fetch_all(&mut *tx)
        .await?;
        if existing.iter().any(|row| row.macro_id != macro_id) {
            return Err(ICloudLinkError::Taken);
        }
        if existing.iter().any(|row| row.removing) {
            return Err(ICloudLinkError::Unavailable);
        }
        let link = match existing.first() {
            Some(row) => {
                let generation = sqlx::query_scalar!(
                    r#"UPDATE email_links SET is_sync_active = true,needs_reauth = false,
                        last_sync_error_at = NULL,updated_at = now()
                    WHERE id = $1 RETURNING sync_generation"#,
                    row.id
                )
                .fetch_one(&mut *tx)
                .await?;
                Saved {
                    id: row.id,
                    sync_generation: generation,
                    started: !row.is_sync_active,
                }
            }
            None => {
                let id = macro_uuid::generate_uuid_v7();
                let generation = sqlx::query_scalar!(
                    r#"INSERT INTO email_links (id,macro_id,fusionauth_user_id,email_address,provider,is_sync_active)
                    VALUES ($1,$2,$3,$4,'ICLOUD',true) RETURNING sync_generation"#,
                    id,
                    macro_id,
                    fusionauth_user_id,
                    address
                )
                .fetch_one(&mut *tx)
                .await?;
                Saved {
                    id,
                    sync_generation: generation,
                    started: true,
                }
            }
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
