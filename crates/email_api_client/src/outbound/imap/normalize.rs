//! Turns a fetched RFC 822 message and its IMAP flags into a mailbox snapshot.

use chrono::{DateTime, Utc};
use mailparse::{DispositionType, MailAddr, MailHeaderMap, ParsedMail};
use models_email::service::{address::ContactInfo, attachment::Attachment, message::Message};
use uuid::Uuid;

use crate::domain::models::{
    Attention, CalendarPart, EmailApiError, FolderRole, MailFolder, MailboxMessage, MailboxState,
    MessageWithCalendarParts, ProviderId,
};

/// The IMAP facts fetched with a message.
pub(super) struct FetchedMessage<'a> {
    /// The mailbox-scoped message identity.
    pub id: &'a ProviderId,
    /// The folder the message is in.
    pub folder: &'a ProviderId,
    /// System flags (`\Seen`) and keywords, as the server wrote them.
    pub flags: Vec<String>,
    /// The server's INTERNALDATE: when the message arrived in the mailbox.
    pub internal_date: Option<DateTime<Utc>>,
    /// The message's MODSEQ, when the server has CONDSTORE.
    pub modseq: Option<u64>,
    /// The complete message source.
    pub raw: &'a [u8],
}

fn malformed() -> EmailApiError {
    EmailApiError::Permanent {
        message: "the IMAP message source does not parse".into(),
    }
}

/// Keywords a client may set or clear as user labels: an atom with no
/// leading `\` (system flag) or `$` (a server or client convention such as
/// `$Junk` or Apple's `$MailFlagBit0`).
pub(super) fn is_label_keyword(flag: &str) -> bool {
    !flag.is_empty() && !flag.starts_with('\\') && !flag.starts_with('$')
}

/// Macro groups a conversation by its root message: the first `References`
/// entry, else `In-Reply-To`, else the message's own `Message-ID`. iCloud
/// has no thread IDs of its own.
pub(super) fn thread_root(mail: &ParsedMail<'_>) -> Option<String> {
    ["References", "In-Reply-To", "Message-ID"]
        .iter()
        .find_map(|header| {
            let value = mail.headers.get_first_value(header)?;
            let start = value.find('<')?;
            let end = start + value[start..].find('>')?;
            Some(value[start..=end].to_owned())
        })
        .filter(|root| !root.chars().any(char::is_control))
}

fn contacts(mail: &ParsedMail<'_>, header: &str) -> Vec<ContactInfo> {
    let Some(header) = mail.headers.get_first_header(header) else {
        return Vec::new();
    };
    let Ok(addresses) = mailparse::addrparse_header(header) else {
        return Vec::new();
    };
    addresses
        .iter()
        .flat_map(|address| match address {
            MailAddr::Single(single) => vec![single.clone()],
            MailAddr::Group(group) => group.addrs.clone(),
        })
        .filter(|single| !single.addr.trim().is_empty())
        .map(|single| ContactInfo {
            email: single.addr,
            name: single.display_name,
            photo_url: None,
        })
        .collect()
}

/// A MIME leaf with its IMAP part number, such as `1.2`.
struct Leaf<'m, 'a> {
    path: String,
    part: &'m ParsedMail<'a>,
}

/// Number parts as IMAP `BODY[section]` does: a multipart's children are
/// 1..n below its own number, and a single-part message is part `1`.
fn leaves<'m, 'a>(part: &'m ParsedMail<'a>, path: String, out: &mut Vec<Leaf<'m, 'a>>) {
    if part.subparts.is_empty() {
        let path = if path.is_empty() { "1".into() } else { path };
        out.push(Leaf { path, part });
        return;
    }
    for (index, child) in part.subparts.iter().enumerate() {
        let child_path = if path.is_empty() {
            (index + 1).to_string()
        } else {
            format!("{path}.{}", index + 1)
        };
        leaves(child, child_path, out);
    }
}

fn filename(part: &ParsedMail<'_>) -> Option<String> {
    part.get_content_disposition()
        .params
        .get("filename")
        .or_else(|| part.ctype.params.get("name"))
        .cloned()
}

/// A part is an attachment when the sender says so or names a file, or
/// when it is a non-text inline part a body refers to by Content-ID.
fn is_attachment(part: &ParsedMail<'_>) -> bool {
    let disposition = part.get_content_disposition().disposition;
    disposition == DispositionType::Attachment
        || filename(part).is_some()
        || (!part.ctype.mimetype.starts_with("text/")
            && part.headers.get_first_value("Content-ID").is_some())
}

/// NOTE: mailparse recurses once per nested multipart with no limit, and a
/// stack overflow aborts the whole process instead of unwinding. Every level
/// needs its own `multipart/` Content-Type, so their count bounds the depth.
/// Past this many a message is refused before parsing; real mail holds a few.
const MAX_MULTIPART: usize = 256;

/// Parse a raw message from any sender, refusing one nested deep enough to
/// overflow the stack.
pub(super) fn parse(raw: &[u8]) -> Option<mailparse::ParsedMail<'_>> {
    let multiparts = raw
        .windows(b"multipart/".len())
        .filter(|window| window.eq_ignore_ascii_case(b"multipart/"))
        .count();
    if multiparts > MAX_MULTIPART {
        return None;
    }
    mailparse::parse_mail(raw).ok()
}

/// The decoded bytes of one attachment, by its part number.
pub(super) fn attachment(raw: &[u8], part_path: &str) -> Result<Option<Vec<u8>>, EmailApiError> {
    let mail = parse(raw).ok_or_else(malformed)?;
    let mut found = Vec::new();
    leaves(&mail, String::new(), &mut found);
    found
        .into_iter()
        .find(|leaf| leaf.path == part_path)
        .map(|leaf| leaf.part.get_body_raw().map_err(|_| malformed()))
        .transpose()
}

/// Build the snapshot the shared ingestion path stores. `owner` is the
/// mailbox's address: mail from it is sent mail wherever it is filed.
pub(super) fn normalize(
    fetched: FetchedMessage<'_>,
    owner: &str,
    link_id: Uuid,
    folders: &[MailFolder],
) -> Result<MailboxMessage, EmailApiError> {
    let mail = parse(fetched.raw).ok_or_else(malformed)?;
    let role = MailFolder::effective_role(folders, Some(fetched.folder));
    let has_flag = |name: &str| fetched.flags.iter().any(|f| f.eq_ignore_ascii_case(name));
    let state = MailboxState {
        is_read: has_flag("\\Seen"),
        is_flagged: has_flag("\\Flagged"),
        is_draft: has_flag("\\Draft") || role == FolderRole::Drafts,
        in_inbox: role == FolderRole::Inbox,
        in_trash: role == FolderRole::Trash,
        in_junk: role == FolderRole::Junk,
        attention: Attention::Unknown,
    };

    let mut parts = Vec::new();
    leaves(&mail, String::new(), &mut parts);
    let mut body_html = None;
    let mut body_text = None;
    let mut attachments = Vec::new();
    let mut calendar_parts = Vec::new();
    for Leaf { path, part } in parts {
        let mime_type = part.ctype.mimetype.to_ascii_lowercase();
        if mime_type == "text/calendar" {
            calendar_parts.push(CalendarPart {
                part_id: Some(path.clone()),
                filename: filename(part),
                mime_type: mime_type.clone(),
                inline_data: Some(part.get_body_raw().map_err(|_| malformed())?),
                provider_attachment_id: None,
            });
        }
        if is_attachment(part) {
            attachments.push(Attachment {
                db_id: Uuid::now_v7(),
                provider_id: Some(path),
                filename: filename(part),
                mime_type: Some(mime_type),
                size_bytes: part
                    .get_body_raw()
                    .ok()
                    .and_then(|bytes| i64::try_from(bytes.len()).ok()),
                content_id: part
                    .headers
                    .get_first_value("Content-ID")
                    .map(|id| id.trim().trim_matches(['<', '>']).to_owned()),
                data_url: None,
                reference_url: None,
                sfs_id: None,
            });
        } else if mime_type == "text/html" && body_html.is_none() {
            body_html = part.get_body().ok();
        } else if mime_type == "text/plain" && body_text.is_none() {
            body_text = part.get_body().ok();
        }
    }
    let body_html_sanitized = body_html.map(|html| email_utils::sanitize_email_html(&html));
    let body_text = match body_text {
        Some(text) => Some(text),
        None => body_html_sanitized
            .as_deref()
            .and_then(email_utils::body_parsed::html_to_plaintext),
    };

    let subject = mail.headers.get_first_value("Subject");
    let from = contacts(&mail, "From").into_iter().next();
    let sent_at = mail
        .headers
        .get_first_value("Date")
        .and_then(|date| mailparse::dateparse(&date).ok())
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0));
    let snippet = email_utils::body_replyless::reply_snippet(
        subject.as_deref(),
        body_html_sanitized.as_deref(),
        body_text.as_deref(),
    );
    let headers = serde_json::Value::Array(
        mail.headers
            .iter()
            .map(
                |header| serde_json::json!({"name": header.get_key(), "value": header.get_value()}),
            )
            .collect(),
    );
    let is_sent = role == FolderRole::Sent
        || (!state.is_draft
            && from
                .as_ref()
                .is_some_and(|from| from.email.eq_ignore_ascii_case(owner)));
    let tags: Vec<String> = fetched
        .flags
        .iter()
        .filter(|flag| is_label_keyword(flag))
        .cloned()
        .collect();
    // Without CONDSTORE the flags are the only state a message can change.
    let version = match fetched.modseq {
        Some(modseq) => modseq.to_string(),
        None => {
            let mut flags = fetched.flags.clone();
            flags.sort();
            flags.join(" ")
        }
    };
    let now = Utc::now();

    let message = Message {
        db_id: Uuid::now_v7(),
        provider_id: Some(fetched.id.as_str().to_owned()),
        thread_db_id: Uuid::now_v7(),
        provider_thread_id: Some(thread_root(&mail).unwrap_or_else(|| fetched.id.as_str().into())),
        replying_to_id: None,
        global_id: mail
            .headers
            .get_first_value("Message-ID")
            .map(|id| id.trim().to_owned()),
        link_id,
        subject,
        snippet,
        provider_history_id: None,
        internal_date_ts: fetched.internal_date.or(sent_at),
        sent_at,
        size_estimate: i64::try_from(fetched.raw.len()).ok(),
        is_read: state.is_read,
        is_starred: state.is_flagged,
        is_sent,
        is_draft: state.is_draft,
        scheduled_send_time: None,
        has_attachments: !attachments.is_empty(),
        from,
        to: contacts(&mail, "To"),
        cc: contacts(&mail, "Cc"),
        bcc: contacts(&mail, "Bcc"),
        labels: Vec::new(),
        body_text,
        body_html_sanitized,
        body_macro: None,
        attachments,
        attachments_draft: Vec::new(),
        attachments_forwarded: Vec::new(),
        headers_json: Some(headers),
        created_at: now,
        updated_at: now,
    };
    Ok(MailboxMessage {
        draft_correlation: None,
        content: MessageWithCalendarParts {
            message,
            calendar_parts,
        },
        calendar_parts_unavailable: false,
        state,
        folder_id: Some(fetched.folder.clone()),
        tags,
        version: Some(version),
    })
}
