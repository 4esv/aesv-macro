//! IMAP and SMTP adapter for mailboxes linked with an address and password,
//! such as iCloud Mail with an app-specific password.
//!
//! It implements the same folder ports as the Microsoft Graph adapter, so the
//! durable folder sync engine drives it: each IMAP folder is a catalog folder
//! with a resolved role, and each folder has its own change stream (see
//! `cursor`). A message's identity is `UIDVALIDITY:UID:folder`, so a moved
//! message is a removal from one stream and an arrival in another, as an
//! Outlook move is.

mod actions;
mod catalog;
mod cursor;
mod normalize;
mod smtp;
mod stream;
#[cfg(test)]
mod test;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_imap::types::{Fetch, Flag, Mailbox, NameAttribute};
use chrono::Utc;
use futures::TryStreamExt;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream};
use tokio_util::either::Either;
use uuid::Uuid;

use self::catalog::ListedFolder;
use self::normalize::FetchedMessage;
use crate::domain::models::{
    AccessToken, EmailApiError, MailFolder, MailboxAccess, MailboxChangePage, MailboxMessage,
    MailboxOrganization, MessageAction, MessageWriteReceipt, ProviderId, StreamToken,
    SubmissionOutcome,
};
use crate::domain::ports::{
    FolderChangeReader, MailboxActionWriter, MailboxContentReader, MailboxRequestCount,
    ScopedMailboxRepository,
};

type Stream = Either<TcpStream, TlsStream<TcpStream>>;
type Session = async_imap::Session<Stream>;

/// How many message IDs one page of a first enumeration lists.
const PAGE_SIZE: usize = 500;
/// Pooled sessions kept per credential; iCloud refuses a burst of logins.
const POOL_SIZE: usize = 4;
/// The longest one provider call may take before it is retried.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// How a connection to a mail server is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// TLS from the first byte (IMAP on 993).
    Tls,
    /// A plaintext connection upgraded with STARTTLS (SMTP submission on 587).
    StartTls,
    /// No TLS. Only for a test server on the same machine.
    Plain,
}

/// One mail server's address.
#[derive(Debug, Clone)]
pub struct Server {
    /// Host name, also the name the TLS certificate must match.
    pub host: String,
    /// TCP port.
    pub port: u16,
    /// How the connection is secured.
    pub security: Security,
}

/// The IMAP server that holds the mailbox and the SMTP server that submits.
#[derive(Debug, Clone)]
pub struct MailServers {
    /// The mailbox's IMAP server. It must offer TLS or no security.
    pub imap: Server,
    /// The submission server.
    pub smtp: Server,
}

impl MailServers {
    /// iCloud Mail's published servers.
    pub fn icloud() -> Self {
        Self {
            imap: Server {
                host: "imap.mail.me.com".into(),
                port: 993,
                security: Security::Tls,
            },
            smtp: Server {
                host: "smtp.mail.me.com".into(),
                port: 587,
                security: Security::StartTls,
            },
        }
    }
}

/// The address and password a mailbox logs in with. It travels through the
/// mailbox token source as an `AccessToken`, so it is held where OAuth access
/// tokens are and never logged.
#[derive(Clone)]
pub struct Credential {
    username: String,
    password: String,
}

impl Credential {
    /// Validate a login. Neither part may be empty or hold a line break,
    /// which would end an IMAP command early.
    pub fn new(username: &str, password: &str) -> Result<Self, EmailApiError> {
        let valid = |value: &str| !value.is_empty() && !value.chars().any(char::is_control);
        if !valid(username) || !valid(password) {
            return Err(EmailApiError::Permanent {
                message: "the mailbox address or password is not valid".into(),
            });
        }
        Ok(Self {
            username: username.to_owned(),
            password: password.to_owned(),
        })
    }

    /// The access token a token source hands this adapter.
    pub fn into_token(self) -> AccessToken {
        AccessToken::new(format!("{}\n{}", self.username, self.password))
    }

    fn from_token(token: &AccessToken) -> Result<Self, EmailApiError> {
        let (username, password) = token
            .expose_secret()
            .split_once('\n')
            .ok_or(EmailApiError::AuthRequired)?;
        Self::new(username, password).map_err(|_| EmailApiError::AuthRequired)
    }
}

/// A logged-in session and what its server supports.
struct Pooled {
    session: Session,
    condstore: bool,
    has_move: bool,
}

/// A folder in IDLE, so the server pushes its changes.
pub struct FolderWatch {
    idle: async_imap::extensions::idle::Handle<Stream>,
}

impl FolderWatch {
    /// Wait up to `timeout` for the server to report a change: new mail, an
    /// expunge or a flag change. True when one came; the caller then makes the
    /// folder's stream due, so the next round reads what changed. iCloud ends
    /// an IDLE after about 30 minutes, so keep `timeout` below that.
    pub async fn changed(&mut self, timeout: Duration) -> Result<bool, EmailApiError> {
        let (wait, _stop) = self.idle.wait_with_timeout(timeout);
        let response = wait.await.map_err(imap_error)?;
        Ok(matches!(
            response,
            async_imap::extensions::idle::IdleResponse::NewData(_)
        ))
    }
}

/// The IMAP and SMTP adapter. Clones share one pool of logged-in sessions.
#[derive(Clone)]
pub struct ImapMailboxRepository {
    servers: MailServers,
    tls: TlsConnector,
    /// Keyed by [`pool_key`], never by the credential itself.
    pool: Arc<Mutex<HashMap<[u8; 32], Vec<Pooled>>>>,
}

fn transient(message: &str) -> EmailApiError {
    EmailApiError::Transient {
        message: message.into(),
    }
}

fn invalid(message: &str) -> EmailApiError {
    EmailApiError::Permanent {
        message: message.into(),
    }
}

/// Server words are left out: a NO can quote a folder or a message.
fn imap_error(error: async_imap::error::Error) -> EmailApiError {
    use async_imap::error::Error;
    match error {
        Error::Io(_) | Error::ConnectionLost => transient("the IMAP connection failed"),
        // iCloud answers NO when it is busy or limits a client; retry later.
        Error::No(_) => transient("the IMAP server refused the command"),
        _ => invalid("the IMAP server rejected the command"),
    }
}

async fn bounded<T>(
    call: impl Future<Output = Result<T, EmailApiError>>,
) -> Result<T, EmailApiError> {
    tokio::time::timeout(CALL_TIMEOUT, call)
        .await
        .map_err(|_| transient("the IMAP server did not answer in time"))?
}

/// `UIDVALIDITY:UID:folder`. The folder goes last because its name may hold `:`.
fn message_id(uid_validity: u32, uid: u32, folder: &str) -> Result<ProviderId, EmailApiError> {
    ProviderId::new(format!("{uid_validity}:{uid}:{folder}"))
}

fn parse_message_id(id: &ProviderId) -> Result<(u32, u32, ProviderId), EmailApiError> {
    let mut parts = id.as_str().splitn(3, ':');
    let parsed = (|| {
        let uid_validity = parts.next()?.parse().ok()?;
        let uid = parts.next()?.parse().ok()?;
        let folder = ProviderId::new(parts.next()?).ok()?;
        Some((uid_validity, uid, folder))
    })();
    parsed.ok_or_else(|| invalid("not an IMAP message identity"))
}

fn flag_name(flag: Flag<'_>) -> String {
    match flag {
        Flag::Seen => "\\Seen".into(),
        Flag::Answered => "\\Answered".into(),
        Flag::Flagged => "\\Flagged".into(),
        Flag::Deleted => "\\Deleted".into(),
        Flag::Draft => "\\Draft".into(),
        Flag::Recent => "\\Recent".into(),
        Flag::MayCreate => "\\*".into(),
        Flag::Custom(name) => name.into_owned(),
    }
}

/// An IMAP quoted string (RFC 3501 4.3).
fn quoted(value: &str) -> Result<String, EmailApiError> {
    if value.chars().any(|c| c == '\r' || c == '\n' || c == '\0') {
        return Err(invalid("a folder name holds a line break"));
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// A keyword must be an IMAP atom to be stored as a label.
fn keyword(tag: &str) -> Result<&str, EmailApiError> {
    let atom = tag.chars().all(|c| {
        c.is_ascii_graphic() && !matches!(c, '(' | ')' | '{' | '%' | '*' | '"' | '\\' | ']')
    });
    if !normalize::is_label_keyword(tag) || !atom {
        return Err(invalid("an iCloud label must be one word without symbols"));
    }
    Ok(tag)
}

/// NOTE: the pool lives as long as the process, so its key is a digest of
/// the credential: the address and app password never sit in it in clear.
fn pool_key(token: &AccessToken) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(token.expose_secret().as_bytes()).into()
}

/// [`Security::Plain`] sends LOGIN and AUTH in clear text, so it is refused
/// for any server but this machine, where only a test server runs.
fn plain_allowed(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

impl ImapMailboxRepository {
    /// An adapter for these servers, trusting the public web roots for TLS.
    pub fn new(servers: MailServers) -> Result<Self, EmailApiError> {
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| invalid("TLS is unavailable"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Self {
            servers,
            tls: TlsConnector::from(Arc::new(config)),
            pool: Arc::default(),
        })
    }

    async fn connect(&self, credential: &Credential) -> Result<Pooled, EmailApiError> {
        let server = &self.servers.imap;
        let tcp = TcpStream::connect((server.host.as_str(), server.port))
            .await
            .map_err(|_| transient("the IMAP server is unreachable"))?;
        let stream = match server.security {
            Security::Plain if plain_allowed(&server.host) => Either::Left(tcp),
            Security::Plain => {
                return Err(invalid("a plain IMAP connection must stay on this machine"));
            }
            Security::Tls => {
                let name = ServerName::try_from(server.host.clone())
                    .map_err(|_| invalid("the IMAP server name is not valid"))?;
                Either::Right(
                    self.tls
                        .connect(name, tcp)
                        .await
                        .map_err(|_| transient("the IMAP TLS handshake failed"))?,
                )
            }
            Security::StartTls => return Err(invalid("IMAP over STARTTLS is not supported")),
        };
        let mut client = async_imap::Client::new(stream);
        client
            .read_response()
            .await
            .map_err(|_| transient("the IMAP server sent no greeting"))?
            .ok_or_else(|| transient("the IMAP server closed the connection"))?;
        let mut session = client
            .login(&credential.username, &credential.password)
            .await
            .map_err(|(error, _)| match error {
                async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_) => {
                    EmailApiError::AuthRequired
                }
                error => imap_error(error),
            })?;
        let capabilities = session.capabilities().await.map_err(imap_error)?;
        Ok(Pooled {
            condstore: capabilities.has_str("CONDSTORE"),
            has_move: capabilities.has_str("MOVE"),
            session,
        })
    }

    /// A pooled session that still answers, or a new login.
    async fn checkout(&self, token: &AccessToken) -> Result<(Credential, Pooled), EmailApiError> {
        let credential = Credential::from_token(token)?;
        loop {
            let pooled = self
                .pool
                .lock()
                .map_err(|_| transient("the IMAP session pool is unavailable"))?
                .get_mut(&pool_key(token))
                .and_then(Vec::pop);
            let Some(mut pooled) = pooled else { break };
            // A connection dropped without a FIN would hang here until TCP gives
            // up, so a NOOP that does not answer soon marks the session dead.
            let alive = tokio::time::timeout(Duration::from_secs(10), pooled.session.noop()).await;
            if matches!(alive, Ok(Ok(_))) {
                return Ok((credential, pooled));
            }
        }
        let pooled = tokio::time::timeout(Duration::from_secs(30), self.connect(&credential))
            .await
            .map_err(|_| transient("the IMAP login did not finish in time"))?;
        if let Err(EmailApiError::AuthRequired) = &pooled {
            // A revoked or changed password: drop that mailbox's sessions.
            if let Ok(mut pool) = self.pool.lock() {
                pool.remove(&pool_key(token));
            }
        }
        Ok((credential, pooled?))
    }

    /// Only a session whose last call succeeded goes back; a failed one may
    /// be mid-response.
    fn checkin<T>(&self, token: &AccessToken, pooled: Pooled, result: &Result<T, EmailApiError>) {
        if result.is_err() {
            return;
        }
        if let Ok(mut pool) = self.pool.lock() {
            let sessions = pool.entry(pool_key(token)).or_default();
            if sessions.len() < POOL_SIZE {
                sessions.push(pooled);
            }
        }
    }

    /// Select a folder. A NO means it is gone.
    async fn select(pooled: &mut Pooled, folder: &str) -> Result<Mailbox, EmailApiError> {
        let selected = if pooled.condstore {
            pooled.session.select_condstore(folder).await
        } else {
            pooled.session.select(folder).await
        };
        selected.map_err(|error| match error {
            async_imap::error::Error::No(_) => EmailApiError::NotFound,
            error => imap_error(error),
        })
    }

    async fn search(pooled: &mut Pooled, query: &str) -> Result<Vec<u32>, EmailApiError> {
        let mut uids: Vec<u32> = pooled
            .session
            .uid_search(query)
            .await
            .map_err(imap_error)?
            .into_iter()
            .collect();
        uids.sort_unstable();
        Ok(uids)
    }

    async fn fetch(
        pooled: &mut Pooled,
        set: &str,
        items: &str,
    ) -> Result<Vec<Fetch>, EmailApiError> {
        pooled
            .session
            .uid_fetch(set, items)
            .await
            .map_err(imap_error)?
            .try_collect()
            .await
            .map_err(imap_error)
    }

    async fn list_folders(pooled: &mut Pooled) -> Result<Vec<MailFolder>, EmailApiError> {
        let names: Vec<_> = pooled
            .session
            .list(Some(""), Some("*"))
            .await
            .map_err(imap_error)?
            .try_collect()
            .await
            .map_err(imap_error)?;
        let mut listed = Vec::new();
        for name in &names {
            let attributes = name.attributes();
            let extension = |wanted: &str| {
                attributes.iter().any(|attribute| {
                    matches!(attribute, NameAttribute::Extension(value) if value.eq_ignore_ascii_case(wanted))
                })
            };
            let no_select = extension("\\NonExistent")
                || attributes
                    .iter()
                    .any(|attribute| matches!(attribute, NameAttribute::NoSelect));
            let total_items = if no_select {
                None
            } else {
                Some(
                    pooled
                        .session
                        .status(name.name(), "(MESSAGES)")
                        .await
                        .map_err(imap_error)?
                        .exists,
                )
            };
            listed.push(ListedFolder {
                name: name.name().to_owned(),
                delimiter: name.delimiter().map(str::to_owned),
                special_use: ListedFolder::special_use(attributes),
                no_select,
                has_children: extension("\\HasChildren"),
                total_items,
            });
        }
        catalog::catalog(&listed)
    }

    /// Fetch one message by identity. `items` names the source section.
    async fn fetch_message(
        pooled: &mut Pooled,
        id: &ProviderId,
        section: &str,
    ) -> Result<Option<(ProviderId, Fetch)>, EmailApiError> {
        let (uid_validity, uid, folder) = parse_message_id(id)?;
        let mailbox = match Self::select(pooled, folder.as_str()).await {
            Err(EmailApiError::NotFound) => return Ok(None),
            selected => selected?,
        };
        if mailbox.uid_validity != Some(uid_validity) {
            return Ok(None);
        }
        let modseq = if pooled.condstore { " MODSEQ" } else { "" };
        let items = format!("(UID FLAGS INTERNALDATE {section}{modseq})");
        let fetch = Self::fetch(pooled, &uid.to_string(), &items)
            .await?
            .into_iter()
            .find(|fetch| fetch.uid == Some(uid));
        Ok(fetch.map(|fetch| (folder, fetch)))
    }

    fn snapshot(
        id: &ProviderId,
        folder: &ProviderId,
        fetch: &Fetch,
        raw: &[u8],
        owner: &str,
        link_id: Uuid,
        folders: &[MailFolder],
    ) -> Result<MailboxMessage, EmailApiError> {
        normalize::normalize(
            FetchedMessage {
                id,
                folder,
                flags: fetch.flags().map(flag_name).collect(),
                internal_date: fetch.internal_date().map(|date| date.with_timezone(&Utc)),
                modseq: fetch.modseq,
                raw,
            },
            owner,
            link_id,
            folders,
        )
    }

    async fn message_in(
        pooled: &mut Pooled,
        owner: &str,
        link_id: Uuid,
        id: &ProviderId,
        folders: &[MailFolder],
        headers_only: bool,
    ) -> Result<Option<MailboxMessage>, EmailApiError> {
        let section = if headers_only {
            "BODY.PEEK[HEADER]"
        } else {
            "BODY.PEEK[]"
        };
        let Some((folder, fetch)) = Self::fetch_message(pooled, id, section).await? else {
            return Ok(None);
        };
        let raw = if headers_only {
            fetch.header()
        } else {
            fetch.body()
        };
        let raw = raw.ok_or_else(|| invalid("the IMAP server sent no message source"))?;
        Self::snapshot(id, &folder, &fetch, raw, owner, link_id, folders).map(Some)
    }

    async fn source(pooled: &mut Pooled, id: &ProviderId) -> Result<Vec<u8>, EmailApiError> {
        let (_, fetch) = Self::fetch_message(pooled, id, "BODY.PEEK[]")
            .await?
            .ok_or(EmailApiError::NotFound)?;
        fetch
            .body()
            .map(<[u8]>::to_vec)
            .ok_or_else(|| invalid("the IMAP server sent no message source"))
    }

    /// Watch one folder with IDLE (RFC 2177) on a connection of its own.
    /// Changes made after this returns are reported by `FolderWatch::changed`.
    pub async fn watch(
        &self,
        token: &AccessToken,
        folder: &ProviderId,
    ) -> Result<FolderWatch, EmailApiError> {
        let (_, mut pooled) = self.checkout(token).await?;
        Self::select(&mut pooled, folder.as_str()).await?;
        let mut idle = pooled.session.idle();
        idle.init().await.map_err(imap_error)?;
        Ok(FolderWatch { idle })
    }
}

impl ScopedMailboxRepository for ImapMailboxRepository {
    /// The credential arrives with each call, so every mailbox shares one adapter.
    fn for_mailbox(&self, _mailbox: MailboxAccess) -> Self {
        self.clone()
    }
}

impl MailboxRequestCount for ImapMailboxRepository {
    /// IMAP calls pass no shared request gate, so there is no count.
    async fn admitted_requests(&self, _mailbox: MailboxAccess) -> Option<u64> {
        None
    }
}

impl FolderChangeReader for ImapMailboxRepository {
    const READER_REVISION: i16 = 1;

    async fn folder_changes(
        &self,
        token: &AccessToken,
        folder: &ProviderId,
        position: Option<&StreamToken>,
    ) -> Result<MailboxChangePage, EmailApiError> {
        let (_, mut pooled) = self.checkout(token).await?;
        let result = bounded(Self::folder_changes_in(&mut pooled, folder, position)).await;
        self.checkin(token, pooled, &result);
        result
    }
}

impl MailboxContentReader for ImapMailboxRepository {
    async fn organization(
        &self,
        token: &AccessToken,
        link_id: Uuid,
        id: &ProviderId,
        folders: &[MailFolder],
    ) -> Result<Option<MailboxOrganization>, EmailApiError> {
        let (credential, mut pooled) = self.checkout(token).await?;
        let owner = credential.username.as_str();
        let call = Self::message_in(&mut pooled, owner, link_id, id, folders, true);
        let result = bounded(call).await;
        self.checkin(token, pooled, &result);
        Ok(result?.map(MailboxOrganization::from))
    }

    async fn folders(&self, token: &AccessToken) -> Result<Vec<MailFolder>, EmailApiError> {
        let (_, mut pooled) = self.checkout(token).await?;
        let result = bounded(Self::list_folders(&mut pooled)).await;
        self.checkin(token, pooled, &result);
        result
    }

    async fn message(
        &self,
        token: &AccessToken,
        link_id: Uuid,
        id: &ProviderId,
        folders: &[MailFolder],
    ) -> Result<Option<MailboxMessage>, EmailApiError> {
        let (credential, mut pooled) = self.checkout(token).await?;
        let owner = credential.username.as_str();
        let call = Self::message_in(&mut pooled, owner, link_id, id, folders, false);
        let result = bounded(call).await;
        self.checkin(token, pooled, &result);
        result
    }

    async fn recent_inbox_messages(
        &self,
        token: &AccessToken,
        link_id: Uuid,
        limit: u16,
        folders: &[MailFolder],
    ) -> Result<Vec<MailboxMessage>, EmailApiError> {
        let (credential, mut pooled) = self.checkout(token).await?;
        let call = async {
            let mailbox = Self::select(&mut pooled, "INBOX").await?;
            let uid_validity = mailbox
                .uid_validity
                .ok_or_else(|| invalid("the IMAP inbox has no UIDVALIDITY"))?;
            let uids = Self::search(&mut pooled, "ALL").await?;
            let newest: Vec<String> = uids
                .iter()
                .rev()
                .take(usize::from(limit))
                .map(u32::to_string)
                .collect();
            if newest.is_empty() {
                return Ok(Vec::new());
            }
            let fetches = Self::fetch(
                &mut pooled,
                &newest.join(","),
                "(UID FLAGS INTERNALDATE BODY.PEEK[HEADER])",
            )
            .await?;
            let inbox = ProviderId::new("INBOX")?;
            let mut messages = Vec::new();
            for fetch in &fetches {
                let (Some(uid), Some(raw)) = (fetch.uid, fetch.header()) else {
                    continue;
                };
                let id = message_id(uid_validity, uid, "INBOX")?;
                let owner = credential.username.as_str();
                messages.push(Self::snapshot(
                    &id, &inbox, fetch, raw, owner, link_id, folders,
                )?);
            }
            messages
                .sort_by_key(|message| std::cmp::Reverse(message.content.message.internal_date_ts));
            Ok(messages)
        };
        let result = bounded(call).await;
        self.checkin(token, pooled, &result);
        result
    }

    async fn attachment(
        &self,
        token: &AccessToken,
        message_id: &ProviderId,
        attachment_id: &ProviderId,
    ) -> Result<Vec<u8>, EmailApiError> {
        let (_, mut pooled) = self.checkout(token).await?;
        let result = bounded(Self::source(&mut pooled, message_id)).await;
        self.checkin(token, pooled, &result);
        normalize::attachment(&result?, attachment_id.as_str())?.ok_or(EmailApiError::NotFound)
    }

    async fn mime(
        &self,
        token: &AccessToken,
        message_id: &ProviderId,
    ) -> Result<Vec<u8>, EmailApiError> {
        let (_, mut pooled) = self.checkout(token).await?;
        let result = bounded(Self::source(&mut pooled, message_id)).await;
        self.checkin(token, pooled, &result);
        result
    }
}

impl MailboxActionWriter for ImapMailboxRepository {
    async fn apply_message_action(
        &self,
        token: &AccessToken,
        message: &ProviderId,
        action: &MessageAction,
        expected_version: Option<&str>,
    ) -> Result<MessageWriteReceipt, EmailApiError> {
        // As for Outlook: replacing labels needs the state they were merged from.
        if matches!(action, MessageAction::SetTags(_)) && expected_version.is_none() {
            return Err(EmailApiError::Conflict);
        }
        let (_, mut pooled) = self.checkout(token).await?;
        let result = bounded(Self::apply_in(&mut pooled, message, action)).await;
        self.checkin(token, pooled, &result);
        result
    }

    async fn submit_draft(
        &self,
        token: &AccessToken,
        draft: &ProviderId,
    ) -> Result<SubmissionOutcome, EmailApiError> {
        let (credential, mut pooled) = self.checkout(token).await?;
        let read = bounded(Self::source(&mut pooled, draft)).await;
        self.checkin(token, pooled, &read);
        let draft_source = read?;
        let outgoing = smtp::outgoing(&draft_source)?;

        let outcome = bounded(smtp::submit(&self.servers.smtp, &credential, &outgoing)).await?;
        if outcome == SubmissionOutcome::Accepted {
            let filed = async {
                let (_, mut pooled) = self.checkout(token).await?;
                // Sent keeps the draft's own source, Bcc lines and all, as Apple
                // Mail does; only the copy that went out over SMTP drops them.
                let result = bounded(Self::file_sent(&mut pooled, draft, &draft_source)).await;
                self.checkin(token, pooled, &result);
                result
            };
            if let Err(error) = filed.await {
                tracing::warn!(%error, "sent iCloud mail was not filed in Sent");
            }
        }
        Ok(outcome)
    }
}
