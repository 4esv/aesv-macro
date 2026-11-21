//! Writes: flags, labels, moves, and filing a sent message.

use async_imap::imap_proto::rfc4315::UidSetMember;
use async_imap::imap_proto::{Response, ResponseCode};
use async_imap::types::Fetch;
use futures::TryStreamExt;

use super::{
    ImapMailboxRepository, Pooled, flag_name, imap_error, invalid, keyword, message_id, normalize,
    parse_message_id, quoted, transient,
};
use crate::domain::models::{
    EmailApiError, FolderRole, MessageAction, MessageWriteReceipt, ProviderId,
};

impl ImapMailboxRepository {
    /// Move with `UID MOVE` (RFC 6851) and read the new UID from the
    /// server's COPYUID answer (RFC 4315).
    pub(super) async fn move_message(
        pooled: &mut Pooled,
        id: &ProviderId,
        destination: &ProviderId,
    ) -> Result<ProviderId, EmailApiError> {
        let (uid_validity, uid, folder) = parse_message_id(id)?;
        if &folder == destination {
            return Ok(id.clone());
        }
        if !pooled.has_move {
            return Err(invalid("the IMAP server cannot move messages"));
        }
        let mailbox = Self::select(pooled, folder.as_str()).await?;
        if mailbox.uid_validity != Some(uid_validity) {
            return Err(EmailApiError::NotFound);
        }
        let command = format!("UID MOVE {uid} {}", quoted(destination.as_str())?);
        let tag = pooled
            .session
            .run_command(&command)
            .await
            .map_err(imap_error)?;
        let mut moved = None;
        loop {
            let response = pooled
                .session
                .read_response()
                .await
                .map_err(|_| transient("the IMAP connection failed"))?
                .ok_or_else(|| transient("the IMAP connection closed"))?;
            let (outcome, done) = match response.parsed() {
                Response::Data { outcome, .. } => (outcome, None),
                Response::Done {
                    tag: done_tag,
                    status,
                    outcome,
                } if *done_tag == tag => (outcome, Some(status)),
                _ => continue,
            };
            if let Some(ResponseCode::CopyUid(new_validity, _, uids)) = &outcome.code {
                moved = match uids.first() {
                    Some(UidSetMember::Uid(new_uid)) => Some((*new_validity, *new_uid)),
                    Some(UidSetMember::UidRange(range)) => Some((*new_validity, *range.start())),
                    None => None,
                };
            }
            match done {
                Some(async_imap::imap_proto::Status::Ok) => break,
                Some(_) => return Err(transient("the IMAP server refused the move")),
                None => {}
            }
        }
        // Without COPYUID the new identity is unknown; the command
        // coordinator reconciles an unknown outcome.
        let (new_validity, new_uid) =
            moved.ok_or_else(|| transient("the IMAP server did not report the moved message"))?;
        message_id(new_validity, new_uid, destination.as_str())
    }

    pub(super) async fn apply_in(
        pooled: &mut Pooled,
        message: &ProviderId,
        action: &MessageAction,
    ) -> Result<MessageWriteReceipt, EmailApiError> {
        let (uid_validity, uid, folder) = parse_message_id(message)?;
        let destination = match action {
            MessageAction::MoveToFolder(destination) => Some(destination.clone()),
            MessageAction::Archive => {
                let folders = Self::list_folders(pooled).await?;
                match folders.into_iter().find(|f| f.role == FolderRole::Archive) {
                    Some(archive) => Some(archive.id),
                    // iCloud's own name, so its other clients see an archive.
                    None => {
                        let created = pooled.session.create("Archive").await;
                        created.map_err(imap_error)?;
                        Some(ProviderId::new("Archive")?)
                    }
                }
            }
            _ => None,
        };
        if let Some(destination) = destination {
            let id = Self::move_message(pooled, message, &destination).await?;
            return Ok(MessageWriteReceipt { id, version: None });
        }

        let mailbox = Self::select(pooled, folder.as_str()).await?;
        if mailbox.uid_validity != Some(uid_validity) {
            return Err(EmailApiError::NotFound);
        }
        let mut stores: Vec<String> = Vec::new();
        match action {
            MessageAction::SetRead(read) => {
                stores.push(format!(
                    "{}FLAGS.SILENT (\\Seen)",
                    if *read { "+" } else { "-" }
                ));
            }
            MessageAction::SetFlagged(flagged) => {
                stores.push(format!(
                    "{}FLAGS.SILENT (\\Flagged)",
                    if *flagged { "+" } else { "-" }
                ));
            }
            MessageAction::SetTags(tags) => {
                let wanted = tags
                    .iter()
                    .map(|tag| keyword(tag))
                    .collect::<Result<Vec<_>, _>>()?;
                let current = Self::fetch(pooled, &uid.to_string(), "(UID FLAGS)").await?;
                let stale: Vec<String> = current
                    .iter()
                    .flat_map(|fetch| fetch.flags().map(flag_name))
                    .filter(|flag| {
                        normalize::is_label_keyword(flag) && !wanted.contains(&flag.as_str())
                    })
                    .collect();
                if !stale.is_empty() {
                    stores.push(format!("-FLAGS.SILENT ({})", stale.join(" ")));
                }
                if !wanted.is_empty() {
                    stores.push(format!("+FLAGS.SILENT ({})", wanted.join(" ")));
                }
            }
            MessageAction::MoveToFolder(_) | MessageAction::Archive => {}
        }
        for store in stores {
            let updates: Vec<Fetch> = pooled
                .session
                .uid_store(uid.to_string(), store)
                .await
                .map_err(imap_error)?
                .try_collect()
                .await
                .map_err(imap_error)?;
            drop(updates);
        }
        Ok(MessageWriteReceipt {
            id: message.clone(),
            version: None,
        })
    }

    /// File the sent copy and remove the draft. The mail is already out, so
    /// a failure here is logged, not returned.
    pub(super) async fn file_sent(
        pooled: &mut Pooled,
        draft: &ProviderId,
        source: &[u8],
    ) -> Result<(), EmailApiError> {
        let folders = Self::list_folders(pooled).await?;
        if let Some(sent) = folders.iter().find(|f| f.role == FolderRole::Sent) {
            pooled
                .session
                .append(sent.id.as_str(), Some("(\\Seen)"), None, source)
                .await
                .map_err(imap_error)?;
        }
        let (uid_validity, uid, folder) = parse_message_id(draft)?;
        let mailbox = Self::select(pooled, folder.as_str()).await?;
        if mailbox.uid_validity == Some(uid_validity) {
            let deleted: Vec<Fetch> = pooled
                .session
                .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
                .await
                .map_err(imap_error)?
                .try_collect()
                .await
                .map_err(imap_error)?;
            drop(deleted);
            let expunged: Vec<_> = pooled
                .session
                .uid_expunge(uid.to_string())
                .await
                .map_err(imap_error)?
                .try_collect()
                .await
                .map_err(imap_error)?;
            drop(expunged);
        }
        Ok(())
    }
}
