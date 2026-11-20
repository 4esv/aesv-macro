//! An iCloud account linked, synced and sent from end to end, against the
//! iCloud-shaped Dovecot and the Mailpit sink that `just icloud-mail-test-servers`
//! starts. Run with `cargo test -p email_service -- --ignored icloud`.

use super::*;
use crate::outbound::email_api::{ICloudApi, ICloudMailbox, ProviderMailboxGateway};
use email::domain::mailbox::MailboxSyncService;
use email::outbound::mailbox_pg::PgMailboxSync;
use email_api_client::domain::models::SendRequest;
use email_api_client::domain::ports::AlwaysAllowRateLimiter;
use email_api_client::outbound::imap::{MailServers, Security, Server};
use macro_db_migrator::MACRO_DB_MIGRATIONS;
use models_email::service::address::ContactInfo;
use models_email::service::link::UserProvider;
use models_email::service::message::MessageToSend;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn test_servers() -> ImapMailboxRepository {
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

/// Deliver `source` to the account's INBOX, as another sender's mail arrives.
async fn deliver(address: &str, source: &str) {
    let stream = tokio::net::TcpStream::connect("127.0.0.1:1143")
        .await
        .expect("connect to the IMAP test server");
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let mut until = async |prefix: &str| loop {
        let line = lines.next_line().await.expect("read").expect("a line");
        if line.starts_with(prefix) {
            assert!(!line.contains(" NO ") && !line.contains(" BAD "), "{line}");
            return;
        }
    };
    until("* OK").await;
    write
        .write_all(format!("a1 LOGIN \"{address}\" \"pass\"\r\n").as_bytes())
        .await
        .expect("login");
    until("a1 ").await;
    write
        .write_all(format!("a2 APPEND INBOX {{{}}}\r\n", source.len()).as_bytes())
        .await
        .expect("append");
    until("+").await;
    write
        .write_all(format!("{source}\r\n").as_bytes())
        .await
        .expect("message");
    until("a2 ").await;
}

/// Run every sync step until none has work left.
async fn sync(
    service: &MailboxSyncService<
        PgMailboxSync,
        ProviderMailboxGateway<ImapMailboxRepository, ICloudEnvCredential, AlwaysAllowRateLimiter>,
        PgMailboxSync,
    >,
) {
    for _ in 0..50 {
        let mut worked = service.discover_once().await.expect("discover");
        worked |= service.enumerate_once().await.expect("enumerate");
        worked |= service.reconcile_once().await.expect("reconcile");
        if !worked {
            return;
        }
    }
    panic!("sync never settled");
}

#[sqlx::test(migrator = "MACRO_DB_MIGRATIONS")]
#[ignore = "needs the IMAP and SMTP test servers: just icloud-mail-test-servers"]
async fn icloud_account_links_syncs_its_inbox_and_sends(db: PgPool) {
    let address = format!("{}@icloud.test", Uuid::new_v4().simple());
    // SAFETY: the only test in this binary that reads or writes these variables.
    unsafe {
        std::env::set_var("ICLOUD_APPLE_ID", &address);
        std::env::set_var("ICLOUD_APP_PASSWORD", "pass");
    }
    let repository = test_servers();
    deliver(
        &address,
        &format!("From: sam@example.com\r\nTo: {address}\r\nSubject: hello from sam\r\nMessage-ID: <hello-{address}>\r\n\r\nHi there.\r\n"),
    )
    .await;

    // The calendar's connect script writes the link with mail sync off.
    let macro_id = format!("macro|{address}");
    let scripted = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO email_links (id,macro_id,fusionauth_user_id,email_address,provider,is_sync_active) VALUES ($1,$2,'owner-id',$3,'ICLOUD',false)",
        scripted,
        macro_id,
        address
    )
    .execute(&db)
    .await
    .expect("scripted link");

    // Link: mail sync turns that row on, and a second user may not take it.
    let links = ICloudLinks {
        db: db.clone(),
        repository: repository.clone(),
    };
    let link_id = links.link(&macro_id, "fusion-icloud").await.expect("link");
    assert_eq!(link_id, scripted);
    assert_eq!(
        links
            .link(&macro_id, "fusion-icloud")
            .await
            .expect("link again"),
        link_id
    );
    assert!(matches!(
        links.link("macro|someone@else.test", "fusion-other").await,
        Err(ICloudLinkError::Taken)
    ));

    // Sync in: the inbox's message lands as a message of the link.
    let pg = PgMailboxSync::new(db.clone()).for_provider(UserProvider::ICloud);
    let mailbox: ICloudMailbox = ICloudMailbox::new(
        repository.clone(),
        ICloudEnvCredential,
        AlwaysAllowRateLimiter,
    );
    let service = MailboxSyncService::new(pg.clone(), ProviderMailboxGateway(mailbox), pg.clone());
    sync(&service).await;
    let inbox = sqlx::query_scalar!(
        "SELECT id FROM email_messages WHERE link_id = $1 AND subject = 'hello from sam'",
        link_id
    )
    .fetch_all(&db)
    .await
    .expect("the synced message");
    assert_eq!(inbox.len(), 1);
    // Outlook's workers do not claim the iCloud link.
    let outlook = PgMailboxSync::new(db.clone());
    assert!(
        email::domain::mailbox::MailboxSyncRepository::claim_catalog(&outlook, Uuid::new_v4())
            .await
            .expect("claim")
            .is_none()
    );

    // Send out: the reply reaches SMTP, is filed in Sent, and sync brings
    // the Sent copy in as the same message, in the same thread.
    let sender = ICloudApi::new(
        repository.clone(),
        ICloudEnvCredential,
        AlwaysAllowRateLimiter,
    );
    let message_id = format!("reply-{}@icloud.test", Uuid::new_v4().simple());
    // The reply threads off the synced message's own headers, as a send does.
    let (parent_message_id, references) =
        crate::util::gmail::send::generate_email_threading_headers(&db, Some(inbox[0])).await;
    assert_eq!(
        parent_message_id.as_deref(),
        Some(format!("hello-{address}").as_str())
    );
    let request = SendRequest {
        message_id: Some(message_id.clone()),
        message: MessageToSend {
            db_id: None,
            provider_id: None,
            replying_to_id: None,
            provider_thread_id: None,
            thread_db_id: None,
            link_id,
            subject: "Re: hello from sam".into(),
            to: Some(vec![ContactInfo {
                email: "sam@example.com".into(),
                ..Default::default()
            }]),
            cc: None,
            bcc: None,
            body_text: Some("Hi Sam.".into()),
            body_html: None,
            body_macro: None,
            attachments: None,
            headers_json: None,
            send_time: None,
        },
        from: ContactInfo {
            email: address.clone(),
            ..Default::default()
        },
        parent_message_id,
        references,
    };
    let prepared = sender
        .prepare_send(link_id, &request, None)
        .await
        .expect("prepare");
    let sent = sender.send_prepared(prepared).await.expect("send");
    assert_eq!(sent.provider_thread_id, format!("<hello-{address}>"));
    assert_eq!(
        sender
            .find_sent_message(link_id, &message_id)
            .await
            .expect("find")
            .expect("the Sent copy")
            .provider_message_id,
        sent.provider_message_id
    );

    // A synced folder polls again two minutes later; let that time pass.
    sqlx::query!(
        "UPDATE email_sync_streams SET next_run_at = now() WHERE link_id = $1",
        link_id
    )
    .execute(&db)
    .await
    .expect("make streams due");
    sync(&service).await;
    let synced = sqlx::query!(
        r#"SELECT m.subject,t.provider_id AS "thread?" FROM email_messages m LEFT JOIN email_threads t ON t.id = m.thread_id
            WHERE m.link_id = $1 AND m.provider_id = $2"#,
        link_id,
        sent.provider_message_id
    )
    .fetch_one(&db)
    .await
    .expect("the Sent copy synced in");
    assert_eq!(synced.subject.as_deref(), Some("Re: hello from sam"));
    assert_eq!(
        synced.thread.as_deref(),
        Some(sent.provider_thread_id.as_str())
    );

    let found: serde_json::Value = reqwest::Client::new()
        .get("http://127.0.0.1:8025/api/v1/search")
        .query(&[("query", format!("message-id:\"{message_id}\""))])
        .send()
        .await
        .expect("search Mailpit")
        .json()
        .await
        .expect("search result");
    assert_eq!(found["messages"].as_array().map(Vec::len), Some(1));
}
