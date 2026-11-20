//! Email API infrastructure adapters.

mod gmail_gate;
mod icloud;
mod microsoft;
mod token_source;

pub use email_api_client::outbound::microsoft_gate::RedisMicrosoftRequestGate;
pub use gmail_gate::{RateBudget, RedisGmailRequestGate};
pub use icloud::ICloudEnvCredential;
pub use microsoft::{MicrosoftCredentialsClient, ProviderMailboxGateway};
pub use token_source::{EmailServiceTokenSource, StaticTokenSource};

use email_api_client::GmailApiClientRepository;
use email_api_client::domain::service::EmailApiClientServiceImpl;

/// Gmail API service composed with email-service infrastructure adapters.
pub type GmailApi = EmailApiClientServiceImpl<
    GmailApiClientRepository,
    EmailServiceTokenSource,
    RedisGmailRequestGate,
>;

/// iCloud Mail's sends, through the scheduled delivery worker as Gmail's go.
pub type ICloudApi = EmailApiClientServiceImpl<
    email_api_client::outbound::imap::ImapMailboxRepository,
    ICloudEnvCredential,
    email_api_client::domain::ports::AlwaysAllowRateLimiter,
>;

/// iCloud Mail's folders, for the durable mailbox sync engine as Outlook's are.
pub type ICloudMailbox = email_api_client::domain::service::mailbox::MailboxApiService<
    email_api_client::outbound::imap::ImapMailboxRepository,
    ICloudEnvCredential,
    email_api_client::domain::ports::AlwaysAllowRateLimiter,
>;

/// iCloud Mail's folders and attachments, behind the mailbox gateway ports.
pub type ICloudGateway = ProviderMailboxGateway<
    email_api_client::outbound::imap::ImapMailboxRepository,
    ICloudEnvCredential,
    email_api_client::domain::ports::AlwaysAllowRateLimiter,
>;

/// The mailbox service over one shared IMAP adapter, whose clones share its sessions.
pub fn icloud_mailbox(
    repository: email_api_client::outbound::imap::ImapMailboxRepository,
) -> ICloudMailbox {
    ICloudMailbox::new(
        repository,
        ICloudEnvCredential,
        email_api_client::domain::ports::AlwaysAllowRateLimiter,
    )
}

/// The send service over one shared IMAP adapter.
pub fn icloud_api(
    repository: email_api_client::outbound::imap::ImapMailboxRepository,
) -> ICloudApi {
    ICloudApi::new(
        repository,
        ICloudEnvCredential,
        email_api_client::domain::ports::AlwaysAllowRateLimiter,
    )
}

/// The IMAP adapter on iCloud's own servers, both over TLS.
pub fn icloud_repository() -> Result<
    email_api_client::outbound::imap::ImapMailboxRepository,
    email_api_client::domain::models::EmailApiError,
> {
    email_api_client::outbound::imap::ImapMailboxRepository::new(
        email_api_client::outbound::imap::MailServers::icloud(),
    )
}
