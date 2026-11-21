//! Direct sends for the scheduled delivery worker: SMTP submission of prepared
//! MIME, a copy filed in Sent, and recovery of that copy by its Message-ID.

use mailparse::MailHeaderMap;

use super::{
    Credential, ImapMailboxRepository, Pooled, bounded, invalid, message_id, normalize, quoted,
    smtp, transient,
};
use crate::domain::models::{AccessToken, EmailApiError, FolderRole, SentIds, SubmissionOutcome};
use crate::domain::ports::MailboxSendRecoveryClient;

/// The Message-ID between its angle brackets, and the thread root sync gives
/// the same message, so the sent row and its synced copy share a thread.
fn identity(mime: &[u8]) -> Result<(String, String), EmailApiError> {
    let mail = normalize::parse(mime).ok_or_else(|| invalid("the message does not parse"))?;
    let header = mail
        .headers
        .get_first_value("Message-ID")
        .ok_or_else(|| invalid("the message has no Message-ID"))?;
    let id = header
        .trim()
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .filter(|id| !id.is_empty() && !id.contains(['<', '>', '"', '\\']))
        .ok_or_else(|| invalid("the message's Message-ID is not valid"))?
        .to_owned();
    let thread = normalize::thread_root(&mail).unwrap_or_else(|| format!("<{id}>"));
    Ok((id, thread))
}

impl ImapMailboxRepository {
    /// The newest copy in Sent whose Message-ID is `id`.
    async fn sent_copy(
        pooled: &mut Pooled,
        id: &str,
    ) -> Result<Option<(u32, u32, String)>, EmailApiError> {
        let folders = Self::list_folders(pooled).await?;
        let Some(sent) = folders.into_iter().find(|f| f.role == FolderRole::Sent) else {
            return Ok(None);
        };
        let mailbox = Self::select(pooled, sent.id.as_str()).await?;
        let uid_validity = mailbox
            .uid_validity
            .ok_or_else(|| invalid("the IMAP server sent no UIDVALIDITY"))?;
        let query = format!("HEADER Message-ID {}", quoted(&format!("<{id}>"))?);
        let uid = Self::search(pooled, &query).await?.last().copied();
        Ok(uid.map(|uid| (uid_validity, uid, sent.id.as_str().to_owned())))
    }

    async fn file_and_find(
        pooled: &mut Pooled,
        mime: &[u8],
        id: &str,
    ) -> Result<Option<(u32, u32, String)>, EmailApiError> {
        let folders = Self::list_folders(pooled).await?;
        let sent = folders
            .iter()
            .find(|f| f.role == FolderRole::Sent)
            .ok_or_else(|| invalid("the mailbox has no Sent folder"))?;
        pooled
            .session
            .append(sent.id.as_str(), Some("(\\Seen)"), None, mime)
            .await
            .map_err(super::imap_error)?;
        Self::sent_copy(pooled, id).await
    }
}

impl MailboxSendRecoveryClient for ImapMailboxRepository {
    async fn send_prepared(
        &self,
        token: &AccessToken,
        mime: &[u8],
        _provider_thread_id: Option<&str>,
    ) -> Result<SentIds, EmailApiError> {
        let credential = Credential::from_token(token)?;
        let (id, thread) = identity(mime)?;
        let outgoing = smtp::outgoing(mime)?;
        match bounded(smtp::submit(&self.servers.smtp, &credential, &outgoing)).await? {
            SubmissionOutcome::Accepted => {}
            // Uncertain, so delivery pauses and reconciles by Message-ID.
            SubmissionOutcome::Unknown => {
                return Err(transient("the SMTP server did not confirm the send"));
            }
        }
        // The mail is out, so a failure to file it must not fail the send.
        let filed = async {
            let (_, mut pooled) = self.checkout(token).await?;
            let result = bounded(Self::file_and_find(&mut pooled, mime, &id)).await;
            self.checkin(token, pooled, &result);
            result
        };
        let provider_message_id = match filed.await {
            Ok(Some((uid_validity, uid, folder))) => {
                message_id(uid_validity, uid, &folder)?.as_str().to_owned()
            }
            other => {
                if let Err(error) = other {
                    tracing::warn!(%error, "sent iCloud mail was not filed in Sent");
                }
                // NOTE: no Sent copy exists for sync to match, so the row keeps
                // the Message-ID; actions on it fail as "not an IMAP identity".
                format!("<{id}>")
            }
        };
        Ok(SentIds {
            provider_message_id,
            provider_thread_id: thread,
        })
    }

    async fn find_sent_message(
        &self,
        token: &AccessToken,
        message_id_header: &str,
    ) -> Result<Option<SentIds>, EmailApiError> {
        let (_, mut pooled) = self.checkout(token).await?;
        let found = bounded(async {
            let Some((uid_validity, uid, folder)) =
                Self::sent_copy(&mut pooled, message_id_header).await?
            else {
                return Ok(None);
            };
            let id = message_id(uid_validity, uid, &folder)?;
            let fetch = Self::fetch_message(&mut pooled, &id, "BODY.PEEK[HEADER]")
                .await?
                .ok_or(EmailApiError::NotFound)?
                .1;
            let header = fetch
                .header()
                .ok_or_else(|| invalid("the IMAP server sent no message header"))?;
            let thread = identity(header)?.1;
            Ok(Some(SentIds {
                provider_message_id: id.as_str().to_owned(),
                provider_thread_id: thread,
            }))
        })
        .await;
        self.checkin(token, pooled, &found);
        found
    }
}
