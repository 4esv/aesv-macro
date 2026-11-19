//! Tests for the IMAP adapter. `server` runs against a real IMAP server.

mod server;

use uuid::Uuid;

use super::catalog::{ListedFolder, catalog};
use super::cursor::FolderCursor;
use super::normalize::{FetchedMessage, attachment, normalize};
use super::{ImapMailboxRepository, message_id, smtp};
use crate::domain::models::{
    EmailApiError, FolderRole, MailFolder, ProviderId, StreamPosition, StreamToken,
};

fn listed(name: &str, special_use: Option<FolderRole>, no_select: bool) -> ListedFolder {
    ListedFolder {
        name: name.into(),
        delimiter: Some("/".into()),
        special_use,
        no_select,
        has_children: false,
        total_items: Some(3),
    }
}

fn role_of(folders: &[MailFolder], name: &str) -> FolderRole {
    folders
        .iter()
        .find(|folder| folder.id.as_str() == name)
        .map(|folder| folder.role)
        .unwrap_or_else(|| panic!("{name} is not in the catalog"))
}

#[test]
fn icloud_folders_map_to_roles_and_a_tree() {
    let folders = catalog(&[
        listed("INBOX", None, false),
        listed("Sent Messages", Some(FolderRole::Sent), false),
        listed("Deleted Messages", Some(FolderRole::Trash), false),
        // A second trash by name only: the announced one keeps the role.
        listed("Trash", None, false),
        listed("Junk", None, false),
        listed("Drafts", None, false),
        listed("Archive", None, false),
        listed("Receipts", None, false),
        listed("Receipts/2026", None, false),
        listed("Shared", None, true),
        listed("Shared/Family", None, false),
        // An iCloud name below the top level is a user folder.
        listed("Projects/Archive", None, false),
    ])
    .expect("catalog");

    assert_eq!(role_of(&folders, "INBOX"), FolderRole::Inbox);
    assert_eq!(role_of(&folders, "Sent Messages"), FolderRole::Sent);
    assert_eq!(role_of(&folders, "Deleted Messages"), FolderRole::Trash);
    assert_eq!(role_of(&folders, "Trash"), FolderRole::Other);
    assert_eq!(role_of(&folders, "Junk"), FolderRole::Junk);
    assert_eq!(role_of(&folders, "Drafts"), FolderRole::Drafts);
    assert_eq!(role_of(&folders, "Archive"), FolderRole::Archive);
    assert_eq!(role_of(&folders, "Receipts"), FolderRole::Other);
    assert_eq!(role_of(&folders, "Projects/Archive"), FolderRole::Other);

    let year = folders
        .iter()
        .find(|folder| folder.id.as_str() == "Receipts/2026")
        .expect("nested folder");
    assert_eq!(year.name, "2026");
    assert_eq!(
        year.parent_id.as_ref().map(ProviderId::as_str),
        Some("Receipts")
    );
    assert!(
        folders
            .iter()
            .find(|folder| folder.id.as_str() == "Receipts")
            .is_some_and(|folder| folder.has_children)
    );

    // A folder that holds no mail is left out; its child hangs from the root.
    assert!(folders.iter().all(|folder| folder.id.as_str() != "Shared"));
    let family = folders
        .iter()
        .find(|folder| folder.id.as_str() == "Shared/Family")
        .expect("child of a no-select folder");
    assert_eq!(family.parent_id, None);
}

#[test]
fn a_cursor_survives_storage_and_keeps_its_uid_set_small() {
    let cursor = FolderCursor {
        uid_validity: 1_794_892_122,
        uid_next: 13,
        highest_modseq: 41,
        uids: vec![1, 2, 3, 4, 7, 9, 10, 11, 12],
        listed_down_to: None,
    };
    let token = cursor.token();
    assert_eq!(token.expose(), "imap1.1794892122.13.41.-.1:4,7,9:12");
    assert_eq!(FolderCursor::parse(&token).expect("parse"), cursor);

    let empty = FolderCursor {
        uids: Vec::new(),
        listed_down_to: Some(500),
        ..cursor
    };
    assert_eq!(FolderCursor::parse(&empty.token()).expect("parse"), empty);

    // Another adapter's token, or a corrupt one, restarts the stream.
    for foreign in ["https://graph.microsoft.com/delta", "imap1.1.2.3.-.4:2"] {
        assert!(matches!(
            FolderCursor::parse(&StreamToken::new(foreign.into())),
            Err(EmailApiError::OutdatedCursor)
        ));
    }
}

#[test]
fn the_first_enumeration_lists_newest_first_in_pages() {
    let cursor = FolderCursor {
        uid_validity: 7,
        uid_next: 1201,
        highest_modseq: 0,
        uids: (1..=1200).collect(),
        listed_down_to: Some(u32::MAX),
    };
    let id = |uid: u32| message_id(7, uid, "INBOX");

    let first = ImapMailboxRepository::list_page(cursor, id).expect("page");
    assert_eq!(first.changed.len(), 500);
    assert_eq!(first.changed[0].as_str(), "7:1200:INBOX");
    let StreamPosition::Continue(next) = first.position else {
        panic!("more pages remain");
    };

    let next = FolderCursor::parse(&next).expect("parse");
    let second = ImapMailboxRepository::list_page(next, id).expect("page");
    assert_eq!(second.changed[0].as_str(), "7:700:INBOX");
    let StreamPosition::Continue(next) = second.position else {
        panic!("more pages remain");
    };

    let next = FolderCursor::parse(&next).expect("parse");
    let third = ImapMailboxRepository::list_page(next, id).expect("page");
    assert_eq!(third.changed.len(), 200);
    assert_eq!(third.changed[199].as_str(), "7:1:INBOX");
    let StreamPosition::Checkpoint(done) = third.position else {
        panic!("the round is complete");
    };
    let done = FolderCursor::parse(&done).expect("parse");
    assert_eq!(done.listed_down_to, None);
    assert_eq!(done.uids.len(), 1200);
    assert_eq!(done.uid_next, 1201);
}

const REPLY: &str = "From: Axel <me@icloud.com>\r
To: Sam <sam@example.com>\r
Subject: Re: Lunch\r
Date: Mon, 16 Nov 2026 12:00:00 -0500\r
Message-ID: <reply-1@icloud.com>\r
In-Reply-To: <second@example.com>\r
References: <root@example.com> <second@example.com>\r
MIME-Version: 1.0\r
Content-Type: multipart/mixed; boundary=\"outer\"\r
\r
--outer\r
Content-Type: multipart/alternative; boundary=\"inner\"\r
\r
--inner\r
Content-Type: text/plain; charset=utf-8\r
\r
See you at noon.\r
--inner\r
Content-Type: text/html; charset=utf-8\r
\r
<p>See you at <b>noon</b>.</p>\r
--inner--\r
--outer\r
Content-Type: application/pdf; name=\"menu.pdf\"\r
Content-Disposition: attachment; filename=\"menu.pdf\"\r
Content-Transfer-Encoding: base64\r
\r
JVBERi0xLjQK\r
--outer--\r
";

fn folders() -> Vec<MailFolder> {
    catalog(&[
        listed("INBOX", None, false),
        listed("Deleted Messages", Some(FolderRole::Trash), false),
    ])
    .expect("catalog")
}

#[test]
fn a_message_maps_its_flags_folder_and_parts() {
    let id = ProviderId::new("9:4:Deleted Messages").expect("id");
    let folder = ProviderId::new("Deleted Messages").expect("folder");
    let snapshot = normalize(
        FetchedMessage {
            id: &id,
            folder: &folder,
            flags: vec![
                "\\Seen".into(),
                "Receipts".into(),
                "$MailFlagBit0".into(),
                "$NotJunk".into(),
            ],
            internal_date: None,
            modseq: Some(17),
            raw: REPLY.as_bytes(),
        },
        "ME@icloud.com",
        Uuid::nil(),
        &folders(),
    )
    .expect("normalize");

    assert!(snapshot.state.is_read);
    assert!(!snapshot.state.is_flagged);
    assert!(snapshot.state.in_trash);
    assert!(!snapshot.state.in_inbox);
    // Keywords become labels; client conventions do not.
    assert_eq!(snapshot.tags, vec!["Receipts".to_owned()]);
    assert_eq!(snapshot.version.as_deref(), Some("17"));
    assert_eq!(snapshot.folder_id.as_ref(), Some(&folder));

    let message = snapshot.content.message;
    // Mail from the mailbox's own address is sent mail wherever it is filed.
    assert!(message.is_sent);
    assert_eq!(
        message.provider_thread_id.as_deref(),
        Some("<root@example.com>")
    );
    assert_eq!(message.global_id.as_deref(), Some("<reply-1@icloud.com>"));
    assert_eq!(message.body_text.as_deref(), Some("See you at noon."));
    assert!(
        message
            .body_html_sanitized
            .is_some_and(|html| html.contains("noon"))
    );
    assert_eq!(message.to[0].email, "sam@example.com");
    assert_eq!(message.attachments.len(), 1);
    assert_eq!(message.attachments[0].provider_id.as_deref(), Some("2"));
    assert_eq!(message.attachments[0].filename.as_deref(), Some("menu.pdf"));

    assert_eq!(
        attachment(REPLY.as_bytes(), "2").expect("attachment"),
        Some(b"%PDF-1.4\n".to_vec())
    );
    assert_eq!(attachment(REPLY.as_bytes(), "9").expect("attachment"), None);
}

#[test]
fn a_sent_message_keeps_bcc_in_the_envelope_only() {
    let draft = "From: me@icloud.com\r
To: sam@example.com\r
Bcc: hidden@example.com,\r
 also-hidden@example.com\r
Subject: Hi\r
\r
Bcc: this line is body text and stays.\r
";
    let outgoing = smtp::outgoing(draft.as_bytes()).expect("outgoing");
    let recipients: Vec<String> = outgoing
        .envelope
        .to()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        recipients,
        [
            "sam@example.com",
            "hidden@example.com",
            "also-hidden@example.com"
        ]
    );
    let source = String::from_utf8(outgoing.source).expect("utf-8");
    assert_eq!(
        source,
        "From: me@icloud.com\r\nTo: sam@example.com\r\nSubject: Hi\r\n\r\nBcc: this line is body text and stays.\r\n"
    );
}

fn nested(depth: usize) -> String {
    let mut message = String::from("From: a@example.com\nTo: b@example.com\n");
    for level in 0..depth {
        message.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=b{level}\n\n--b{level}\n"
        ));
    }
    message.push_str("Content-Type: text/plain\n\nhi\n");
    message
}

#[test]
fn deeply_nested_mime_is_refused_before_it_can_overflow_the_stack() {
    let deep = nested(5_000);
    assert!(super::normalize::parse(deep.as_bytes()).is_none());
    assert!(super::normalize::attachment(deep.as_bytes(), "1").is_err());
    assert!(super::smtp::outgoing(deep.as_bytes()).is_err());
    assert!(super::normalize::parse(nested(10).as_bytes()).is_some());
}

#[test]
fn a_plain_connection_is_allowed_only_to_this_machine() {
    for host in ["127.0.0.1", "::1", "localhost"] {
        assert!(super::plain_allowed(host), "{host}");
    }
    for host in ["imap.mail.me.com", "10.0.0.5", "localhost.evil.example"] {
        assert!(!super::plain_allowed(host), "{host}");
    }
}
