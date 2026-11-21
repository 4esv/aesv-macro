//! The opaque per-folder sync position an IMAP folder stream stores.

use crate::domain::models::{EmailApiError, StreamToken};

/// What one folder looked like when a round started.
///
/// IMAP reports new mail by UID and flag changes by MODSEQ (CONDSTORE), but
/// a server without QRESYNC never says which UIDs were expunged. The cursor
/// therefore keeps the folder's UID set, and a round that finds fewer
/// messages than it expects compares the set against a fresh `UID SEARCH`.
/// Ranges keep the set small: a folder of contiguous UIDs costs a few bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FolderCursor {
    /// The folder's UIDVALIDITY; a change voids every UID below.
    pub uid_validity: u32,
    /// UIDs at or above this arrived after the cursor was taken.
    pub uid_next: u32,
    /// The folder's HIGHESTMODSEQ, or 0 when the server has no CONDSTORE.
    pub highest_modseq: u64,
    /// Every UID the stream has listed, ascending.
    pub uids: Vec<u32>,
    /// During the first enumeration: UIDs below this one are still unlisted.
    pub listed_down_to: Option<u32>,
}

const VERSION: &str = "imap1";

impl FolderCursor {
    /// Serialize for storage in the stream's position.
    pub fn token(&self) -> StreamToken {
        let listed = self
            .listed_down_to
            .map_or_else(|| "-".to_owned(), |uid| uid.to_string());
        StreamToken::new(format!(
            "{VERSION}.{}.{}.{}.{listed}.{}",
            self.uid_validity,
            self.uid_next,
            self.highest_modseq,
            sequence_set(&self.uids)
        ))
    }

    /// Read a stored position. Anything this version did not write is an
    /// outdated cursor, so the stream starts a fresh enumeration.
    pub fn parse(token: &StreamToken) -> Result<Self, EmailApiError> {
        Self::parse_parts(token.expose()).ok_or(EmailApiError::OutdatedCursor)
    }

    fn parse_parts(value: &str) -> Option<Self> {
        let mut parts = value.splitn(6, '.');
        if parts.next()? != VERSION {
            return None;
        }
        let uid_validity = parts.next()?.parse().ok()?;
        let uid_next = parts.next()?.parse().ok()?;
        let highest_modseq = parts.next()?.parse().ok()?;
        let listed_down_to = match parts.next()? {
            "-" => None,
            uid => Some(uid.parse().ok()?),
        };
        let uids = parse_sequence_set(parts.next()?)?;
        Some(Self {
            uid_validity,
            uid_next,
            highest_modseq,
            uids,
            listed_down_to,
        })
    }
}

/// Write ascending UIDs as an IMAP sequence set, such as `1:4,7,9:12`.
pub(super) fn sequence_set(uids: &[u32]) -> String {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for &uid in uids {
        match ranges.last_mut() {
            Some((_, end)) if end.checked_add(1) == Some(uid) => *end = uid,
            _ => ranges.push((uid, uid)),
        }
    }
    ranges
        .iter()
        .map(|&(start, end)| {
            if start == end {
                start.to_string()
            } else {
                format!("{start}:{end}")
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Read a sequence set written by `sequence_set`; ascending and deduplicated.
fn parse_sequence_set(value: &str) -> Option<Vec<u32>> {
    let mut uids = Vec::new();
    for range in value.split(',').filter(|range| !range.is_empty()) {
        let (start, end) = match range.split_once(':') {
            Some((start, end)) => (start.parse().ok()?, end.parse().ok()?),
            None => {
                let uid = range.parse().ok()?;
                (uid, uid)
            }
        };
        if start > end || uids.last().is_some_and(|&last| last >= start) {
            return None;
        }
        uids.extend(start..=end);
    }
    Some(uids)
}
