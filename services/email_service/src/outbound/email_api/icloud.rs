//! The iCloud Mail login, read from the stack's environment.

use email::domain::mailbox::credentials::{CredentialHealth, MailboxCredentialHealthRepository};
use email::outbound::mailbox_pg::PgMailboxSync;
use email_api_client::domain::models::EmailApiError;
use email_api_client::domain::models::{AccessToken, MailboxAccess, TokenError, TokenFreshness};
use email_api_client::domain::ports::MailboxContentReader;
use email_api_client::domain::ports::{MailboxTokenSource, ProviderTokenSource};
use email_api_client::outbound::imap::{Credential, ImapMailboxRepository};
use models_email::service::link::Link;
use uuid::Uuid;

/// The Apple ID and app-specific password every iCloud link logs in with:
/// the stack's one iCloud login, as the calendar reads it.
#[derive(Clone, Default)]
pub struct ICloudEnvCredential {
    verifier: Option<(ImapMailboxRepository, PgMailboxSync)>,
}

impl ICloudEnvCredential {
    /// A credential that logs in on a fresh request, which follows every
    /// refused call, and marks the link for reauthorization when iCloud refuses.
    pub fn verified(repository: ImapMailboxRepository, health: PgMailboxSync) -> Self {
        Self {
            verifier: Some((repository, health)),
        }
    }

    // NOTE(shortcut): one iCloud account from env, a per-link encrypted store when a second account matters.
    fn login() -> Option<(String, String)> {
        // The mailbox's address is the Apple ID: `ICloudLinks` turns on only that row.
        let apple_id = std::env::var("ICLOUD_APPLE_ID").ok()?;
        let login =
            calendar_events::outbound::caldav::CalDavCalendarClient::stack_credential(&apple_id)?;
        // An Apple ID holds no `:`, so the first one ends it.
        let (apple_id, password) = login.split_once(':')?;
        Some((apple_id.to_owned(), password.to_owned()))
    }

    /// The configured Apple ID, which is also the linked mailbox's address.
    pub fn apple_id() -> Option<String> {
        Self::login().map(|(apple_id, _)| apple_id)
    }

    /// The login as the IMAP adapter's token. Read at each use, so a changed
    /// password takes effect without a restart. The password is never logged.
    pub fn token() -> Result<AccessToken, TokenError> {
        let (apple_id, password) = Self::login().ok_or(TokenError::ReauthRequired)?;
        Credential::new(&apple_id, &password)
            .map(Credential::into_token)
            .map_err(|_| TokenError::ReauthRequired)
    }
}

impl MailboxTokenSource for ICloudEnvCredential {
    async fn access_token(
        &self,
        mailbox: MailboxAccess,
        freshness: TokenFreshness,
    ) -> Result<AccessToken, TokenError> {
        let (Some((repository, health)), TokenFreshness::Fresh) = (&self.verifier, freshness)
        else {
            return Self::token();
        };
        if let Ok(token) = Self::token() {
            match repository.folders(&token).await {
                Err(EmailApiError::AuthRequired) => {}
                // Any other answer says nothing about the password.
                _ => return Ok(token),
            }
        }
        health
            .record(mailbox, CredentialHealth::ReauthorizationRequired)
            .await?;
        Err(TokenError::ReauthRequired)
    }
}

impl ProviderTokenSource for ICloudEnvCredential {
    async fn get_access_token(
        &self,
        _link_id: Uuid,
        _freshness: TokenFreshness,
    ) -> Result<AccessToken, TokenError> {
        Self::token()
    }

    async fn get_access_token_for_link(
        &self,
        _link: &Link,
        _freshness: TokenFreshness,
    ) -> Result<AccessToken, TokenError> {
        Self::token()
    }

    async fn get_access_token_health_neutral(
        &self,
        _link: &Link,
        _freshness: TokenFreshness,
    ) -> Result<AccessToken, TokenError> {
        Self::token()
    }
}
