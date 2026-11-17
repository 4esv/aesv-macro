//! The adapter against a real IMAP server shaped like iCloud Mail (Dovecot with
//! `fixtures/imap/dovecot.conf`) and a Mailpit SMTP sink. Start both with
//! `just icloud-mail-test-servers`, then run
//! `cargo test -p email_api_client --features outbound-imap -- --ignored imap`.
//! Each test logs in as a new user, so it starts from an empty mailbox.

use std::collections::BTreeSet;
use std::time::Duration;

use futures::TryStreamExt;
use uuid::Uuid;

use super::super::cursor::FolderCursor;
use super::super::{Credential, ImapMailboxRepository, MailServers, Pooled, Security, Server};
use crate::domain::models::{
    AccessToken, EmailApiError, FolderRole, MailFolder, MessageAction, ProviderId, StreamPosition,
    StreamToken, SubmissionOutcome,
};
use crate::domain::ports::{FolderChangeReader, MailboxActionWriter, MailboxContentReader};

fn repository() -> ImapMailboxRepository {
    ImapMailboxRepository::new(MailServers {
        imap: Server {
            host: "127.0.0.1".into(),
            port: 1143,
            security: Security::Plain,
        },
        smtp: Server {
            host: "127.0.0.1".into(),
            port: 1025,
            security: Security::Plain,
        },
    })
    .expect("repository")
}

fn new_mailbox() -> (String, AccessToken) {
    let address = format!("{}@icloud.test", Uuid::new_v4().simple());
    let token = Credential::new(&address, "pass")
        .expect("credential")
        .into_token();
    (address, token)
}

fn mail(from: &str, subject: &str) -> String {
    format!(
        "From: {from}\r\nTo: sam@example.com\r\nSubject: {subject}\r\nMessage-ID: <{}@example.com>\r\n\r\nHello.\r\n",
        Uuid::new_v4().simple()
    )
}

/// A second session of the same user, standing in for another mail client.
async fn other_client(repository: &ImapMailboxRepository, token: &AccessToken) -> Pooled {
    repository.checkout(token).await.expect("login").1
}

async fn append(client: &mut Pooled, folder: &str, flags: Option<&str>, source: &str) {
    client
        .session
        .append(folder, flags, None, source)
        .await
        .expect("append");
}

async fn store(client: &mut Pooled, folder: &str, uid: u32, change: &str) {
    client.session.select(folder).await.expect("select");
    let _: Vec<_> = client
        .session
        .uid_store(uid.to_string(), change)
        .await
        .expect("store")
        .try_collect()
        .await
        .expect("store responses");
}

fn uids(ids: &[ProviderId]) -> BTreeSet<u32> {
    ids.iter()
        .map(|id| {
            let mut parts = id.as_str().splitn(3, ':');
            parts.next();
            parts.next().and_then(|uid| uid.parse().ok()).expect("uid")
        })
        .collect()
}

fn checkpoint(position: StreamPosition) -> StreamToken {
    match position {
        StreamPosition::Checkpoint(token) => token,
        StreamPosition::Continue(_) => panic!("a small folder lists in one page"),
    }
}

fn folder<'a>(folders: &'a [MailFolder], name: &str) -> &'a MailFolder {
    folders
        .iter()
        .find(|folder| folder.id.as_str() == name)
        .unwrap_or_else(|| panic!("{name} is not in the catalog"))
}

#[tokio::test]
#[ignore = "needs the IMAP test server: just icloud-mail-test-servers"]
async fn imap_folders_map_to_catalog_roles() {
    let repository = repository();
    let (address, token) = new_mailbox();
    let mut client = other_client(&repository, &token).await;
    client.session.create("Receipts").await.expect("create");
    client
        .session
        .create("Receipts/2026")
        .await
        .expect("create");
    append(&mut client, "INBOX", None, &mail("sam@example.com", "one")).await;
    append(&mut client, "INBOX", None, &mail("sam@example.com", "two")).await;
    append(
        &mut client,
        "Receipts/2026",
        None,
        &mail(&address, "receipt"),
    )
    .await;

    let folders = repository.folders(&token).await.expect("folders");

    assert_eq!(folder(&folders, "INBOX").role, FolderRole::Inbox);
    assert_eq!(folder(&folders, "INBOX").total_items, Some(2));
    assert_eq!(folder(&folders, "Drafts").role, FolderRole::Drafts);
    assert_eq!(folder(&folders, "Sent Messages").role, FolderRole::Sent);
    assert_eq!(folder(&folders, "Deleted Messages").role, FolderRole::Trash);
    assert_eq!(folder(&folders, "Junk").role, FolderRole::Junk);
    // The fixture gives Archive no special-use flag; iCloud's name resolves it.
    assert_eq!(folder(&folders, "Archive").role, FolderRole::Archive);
    assert_eq!(folder(&folders, "Receipts").role, FolderRole::Other);
    assert!(folder(&folders, "Receipts").has_children);
    let year = folder(&folders, "Receipts/2026");
    assert_eq!(year.role, FolderRole::Other);
    assert_eq!(year.name, "2026");
    assert_eq!(
        year.parent_id.as_ref().map(ProviderId::as_str),
        Some("Receipts")
    );
    assert_eq!(year.total_items, Some(1));
}

#[tokio::test]
#[ignore = "needs the IMAP test server: just icloud-mail-test-servers"]
async fn imap_stream_syncs_new_changed_and_expunged_mail_incrementally() {
    let repository = repository();
    let (_, token) = new_mailbox();
    let inbox = ProviderId::new("INBOX").expect("folder");
    let mut client = other_client(&repository, &token).await;
    for subject in ["one", "two", "three"] {
        append(
            &mut client,
            "INBOX",
            None,
            &mail("sam@example.com", subject),
        )
        .await;
    }

    // The first round lists every message, newest first.
    let first = repository
        .folder_changes(&token, &inbox, None)
        .await
        .expect("first round");
    assert_eq!(uids(&first.changed), BTreeSet::from([1, 2, 3]));
    assert_eq!(first.changed[0].as_str().split(':').nth(1), Some("3"));
    assert!(first.removed.is_empty());
    let position = checkpoint(first.position);

    // Nothing changed, so a round reads nothing.
    let idle = repository
        .folder_changes(&token, &inbox, Some(&position))
        .await
        .expect("idle round");
    assert!(idle.changed.is_empty() && idle.removed.is_empty());
    let position = checkpoint(idle.position);

    // Another client: mail arrives, one is read, one is deleted.
    append(&mut client, "INBOX", None, &mail("sam@example.com", "four")).await;
    store(&mut client, "INBOX", 1, "+FLAGS (\\Seen)").await;
    store(&mut client, "INBOX", 2, "+FLAGS (\\Deleted)").await;
    let _: Vec<_> = client
        .session
        .uid_expunge("2")
        .await
        .expect("expunge")
        .try_collect()
        .await
        .expect("expunge responses");

    let round = repository
        .folder_changes(&token, &inbox, Some(&position))
        .await
        .expect("incremental round");
    assert_eq!(uids(&round.changed), BTreeSet::from([1, 4]));
    assert_eq!(uids(&round.removed), BTreeSet::from([2]));
    let position = checkpoint(round.position);

    let read = repository
        .message(&token, Uuid::nil(), &round.changed[0], &[])
        .await
        .expect("message")
        .expect("message 1 is present");
    assert!(read.state.is_read);
    let gone = repository
        .message(&token, Uuid::nil(), &round.removed[0], &[])
        .await
        .expect("message");
    assert!(gone.is_none());

    let quiet = repository
        .folder_changes(&token, &inbox, Some(&position))
        .await
        .expect("quiet round");
    assert!(quiet.changed.is_empty() && quiet.removed.is_empty());

    // A recreated folder has a new UIDVALIDITY: every stored UID is void.
    let mut stale = FolderCursor::parse(&position).expect("cursor");
    stale.uid_validity += 1;
    let reset = repository
        .folder_changes(&token, &inbox, Some(&stale.token()))
        .await;
    assert!(matches!(reset, Err(EmailApiError::OutdatedCursor)));
}

#[tokio::test]
#[ignore = "needs the IMAP test server: just icloud-mail-test-servers"]
async fn imap_writes_reach_the_server() {
    let repository = repository();
    let (_, token) = new_mailbox();
    let mut client = other_client(&repository, &token).await;
    client.session.create("Receipts").await.expect("create");
    append(
        &mut client,
        "INBOX",
        None,
        &mail("shop@example.com", "order"),
    )
    .await;
    let folders = repository.folders(&token).await.expect("folders");
    let id = ProviderId::new(format!(
        "{}:1:INBOX",
        client
            .session
            .select("INBOX")
            .await
            .expect("select")
            .uid_validity
            .expect("uidvalidity")
    ))
    .expect("id");

    for action in [
        MessageAction::SetRead(true),
        MessageAction::SetFlagged(true),
        MessageAction::SetTags(vec!["Receipts".into(), "Orders".into()]),
    ] {
        let receipt = repository
            .apply_message_action(&token, &id, &action, Some("1"))
            .await
            .expect("write");
        assert_eq!(receipt.id, id);
    }
    // Labels are replaced, not added to.
    repository
        .apply_message_action(
            &token,
            &id,
            &MessageAction::SetTags(vec!["Orders".into()]),
            Some("1"),
        )
        .await
        .expect("write");

    let destination = ProviderId::new("Receipts").expect("folder");
    let moved = repository
        .apply_message_action(
            &token,
            &id,
            &MessageAction::MoveToFolder(destination.clone()),
            None,
        )
        .await
        .expect("move")
        .id;
    assert!(moved.as_str().ends_with(":Receipts"));
    let old = repository.message(&token, Uuid::nil(), &id, &folders).await;
    assert!(old.expect("message").is_none());

    let message = repository
        .message(&token, Uuid::nil(), &moved, &folders)
        .await
        .expect("message")
        .expect("the moved message is present");
    assert_eq!(message.folder_id, Some(destination));
    assert!(message.state.is_read && message.state.is_flagged);
    assert!(!message.state.in_inbox);
    assert_eq!(message.tags, vec!["Orders".to_owned()]);

    let archived = repository
        .apply_message_action(&token, &moved, &MessageAction::Archive, None)
        .await
        .expect("archive")
        .id;
    assert!(archived.as_str().ends_with(":Archive"));
}

#[tokio::test]
#[ignore = "needs the IMAP test server: just icloud-mail-test-servers"]
async fn imap_idle_reports_new_mail() {
    let repository = repository();
    let (_, token) = new_mailbox();
    let inbox = ProviderId::new("INBOX").expect("folder");
    let mut watch = repository.watch(&token, &inbox).await.expect("watch");

    let mut client = other_client(&repository, &token).await;
    append(
        &mut client,
        "INBOX",
        None,
        &mail("sam@example.com", "pushed"),
    )
    .await;

    assert!(watch.changed(Duration::from_secs(30)).await.expect("idle"));
}

#[derive(serde::Deserialize)]
struct MailpitSearch {
    messages: Vec<MailpitMessage>,
}

#[derive(serde::Deserialize)]
struct MailpitMessage {
    #[serde(rename = "ID")]
    id: String,
    /// Envelope recipients missing from the headers.
    #[serde(rename = "Bcc")]
    bcc: Vec<MailpitAddress>,
}

#[derive(serde::Deserialize)]
struct MailpitAddress {
    #[serde(rename = "Address")]
    address: String,
}

#[tokio::test]
#[ignore = "needs the IMAP and SMTP test servers: just icloud-mail-test-servers"]
async fn imap_draft_is_sent_over_smtp_and_filed_in_sent() {
    let repository = repository();
    let (address, token) = new_mailbox();
    let subject = format!("draft {}", Uuid::new_v4().simple());
    let draft = format!(
        "From: {address}\r\nTo: sam@example.com\r\nBcc: hidden@example.com\r\nSubject: {subject}\r\n\r\nSent from Macro.\r\n"
    );
    let mut client = other_client(&repository, &token).await;
    append(&mut client, "Drafts", Some("(\\Draft)"), &draft).await;
    let validity = client
        .session
        .select("Drafts")
        .await
        .expect("select")
        .uid_validity;
    let id = ProviderId::new(format!("{}:1:Drafts", validity.expect("uidvalidity"))).expect("id");

    let outcome = repository.submit_draft(&token, &id).await.expect("submit");
    assert_eq!(outcome, SubmissionOutcome::Accepted);

    let folders = repository.folders(&token).await.expect("folders");
    assert_eq!(folder(&folders, "Drafts").total_items, Some(0));
    assert_eq!(folder(&folders, "Sent Messages").total_items, Some(1));

    let http = reqwest::Client::new();
    let found: MailpitSearch = http
        .get("http://127.0.0.1:8025/api/v1/search")
        .query(&[("query", format!("subject:\"{subject}\""))])
        .send()
        .await
        .expect("search Mailpit")
        .json()
        .await
        .expect("search result");
    assert_eq!(found.messages.len(), 1);
    assert_eq!(found.messages[0].bcc[0].address, "hidden@example.com");
    let raw = http
        .get(format!(
            "http://127.0.0.1:8025/api/v1/message/{}/raw",
            found.messages[0].id
        ))
        .send()
        .await
        .expect("read Mailpit")
        .text()
        .await
        .expect("raw message");
    // Mailpit writes its own headers, a Bcc line among them, above what it received.
    let received = &raw[raw.find("From: ").expect("the sent headers")..];
    assert!(received.contains("Sent from Macro."));
    assert!(!received.contains("hidden@example.com"));
}
