//! A folder's change stream: the first enumeration and the rounds after it.

use std::collections::BTreeSet;

use async_imap::types::Mailbox;

use super::cursor::FolderCursor;
use super::{ImapMailboxRepository, PAGE_SIZE, Pooled, invalid, message_id};
use crate::domain::models::{
    EmailApiError, MailboxChangePage, ProviderId, StreamPosition, StreamToken,
};
use crate::domain::ports::FolderChangeReader;

impl ImapMailboxRepository {
    /// One page of a folder's stream; see `FolderCursor` for the method.
    pub(super) async fn folder_changes_in(
        pooled: &mut Pooled,
        folder: &ProviderId,
        position: Option<&StreamToken>,
    ) -> Result<MailboxChangePage, EmailApiError> {
        let cursor = position.map(FolderCursor::parse).transpose()?;
        let mailbox = Self::select(pooled, folder.as_str()).await?;
        let uid_validity = mailbox
            .uid_validity
            .ok_or_else(|| invalid("the IMAP folder has no UIDVALIDITY"))?;
        let highest_modseq = match pooled.condstore {
            true => mailbox.highest_modseq.unwrap_or(0),
            false => 0,
        };
        let id = |uid: u32| message_id(uid_validity, uid, folder.as_str());

        let cursor = match cursor {
            Some(cursor) if cursor.uid_validity != uid_validity => {
                return Err(EmailApiError::OutdatedCursor);
            }
            Some(cursor) if cursor.listed_down_to.is_some() => cursor,
            Some(cursor) => {
                return Self::catch_up(pooled, &mailbox, cursor, highest_modseq, id).await;
            }
            // A new round snapshots the folder. Mail that arrives later is
            // at or above `uid_next`, so the next round lists it.
            None => {
                let uids = Self::search(pooled, "ALL").await?;
                let uid_next = mailbox
                    .uid_next
                    .unwrap_or_else(|| uids.last().map_or(1, |uid| uid + 1));
                FolderCursor {
                    uid_validity,
                    uid_next,
                    highest_modseq,
                    uids,
                    listed_down_to: Some(u32::MAX),
                }
            }
        };
        Self::list_page(cursor, id)
    }

    /// The first enumeration lists the snapshot newest first, a page at a time.
    pub(super) fn list_page(
        mut cursor: FolderCursor,
        id: impl Fn(u32) -> Result<ProviderId, EmailApiError>,
    ) -> Result<MailboxChangePage, EmailApiError> {
        let down_to = cursor.listed_down_to.unwrap_or(u32::MAX);
        let page: Vec<u32> = cursor
            .uids
            .iter()
            .rev()
            .copied()
            .filter(|uid| *uid < down_to)
            .take(PAGE_SIZE)
            .collect();
        let lowest = page.last().copied().unwrap_or(0);
        let more = cursor.uids.iter().any(|uid| *uid < lowest);
        cursor.listed_down_to = more.then_some(lowest);
        let token = cursor.token();
        Ok(MailboxChangePage {
            changed: page.into_iter().map(id).collect::<Result<_, _>>()?,
            removed: Vec::new(),
            position: match more {
                true => StreamPosition::Continue(token),
                false => StreamPosition::Checkpoint(token),
            },
            reader_revision: Self::READER_REVISION,
        })
    }

    /// A caught-up round: new UIDs, flag changes since the last MODSEQ, and,
    /// when the message count shows some left, which ones.
    pub(super) async fn catch_up(
        pooled: &mut Pooled,
        mailbox: &Mailbox,
        mut cursor: FolderCursor,
        highest_modseq: u64,
        id: impl Fn(u32) -> Result<ProviderId, EmailApiError>,
    ) -> Result<MailboxChangePage, EmailApiError> {
        let arrived: Vec<u32> = match mailbox.uid_next {
            Some(uid_next) if uid_next <= cursor.uid_next => Vec::new(),
            // `n:*` always matches the newest message, even below n.
            _ => Self::search(pooled, &format!("UID {}:*", cursor.uid_next))
                .await?
                .into_iter()
                .filter(|uid| *uid >= cursor.uid_next)
                .collect(),
        };
        let mut changed: BTreeSet<u32> = arrived.iter().copied().collect();

        // NOTE(shortcut): without CONDSTORE a flag change made elsewhere is
        // seen only when the stream resets; add a FLAGS comparison if a server
        // in use lacks CONDSTORE. Dovecot has it; iCloud is unverified.
        let flags_changed = cursor.highest_modseq > 0 && highest_modseq > cursor.highest_modseq;
        if flags_changed && mailbox.exists > 0 {
            let query = format!("(UID FLAGS) (CHANGEDSINCE {})", cursor.highest_modseq);
            for fetch in Self::fetch(pooled, "1:*", &query).await? {
                changed.extend(fetch.uid);
            }
        }

        let expected = cursor.uids.len() + arrived.len();
        let mut removed = Vec::new();
        if usize::try_from(mailbox.exists).ok() != Some(expected) {
            let now = Self::search(pooled, "ALL").await?;
            let present: BTreeSet<u32> = now.iter().copied().collect();
            removed = cursor
                .uids
                .iter()
                .copied()
                .filter(|uid| !present.contains(uid))
                .collect();
            // A message that arrived between SELECT and the search is new too.
            changed.extend(now.iter().copied().filter(|uid| *uid >= cursor.uid_next));
            cursor.uids = now;
        } else {
            cursor.uids.extend(&arrived);
        }
        changed.retain(|uid| cursor.uids.binary_search(uid).is_ok());

        let newest = cursor.uids.last().map_or(0, |uid| uid + 1);
        cursor.uid_next = mailbox
            .uid_next
            .unwrap_or(0)
            .max(newest)
            .max(cursor.uid_next);
        cursor.highest_modseq = highest_modseq;
        Ok(MailboxChangePage {
            changed: changed.into_iter().map(&id).collect::<Result<_, _>>()?,
            removed: removed.into_iter().map(&id).collect::<Result<_, _>>()?,
            position: StreamPosition::Checkpoint(cursor.token()),
            reader_revision: Self::READER_REVISION,
        })
    }
}
