//! The iCloud link route, and an iCloud account linked, synced and sent from
//! end to end against the iCloud-shaped Dovecot and the Mailpit sink that
//! `just icloud-mail-test-servers` starts. Run the end-to-end test with
//! `cargo test -p email_service -- --ignored icloud`.

use super::*;
use crate::outbound::email_api::{ICloudApi, ICloudEnvCredential, ProviderMailboxGateway};
use axum::body::Body;
use axum::http::Request;
use email::domain::mailbox::MailboxSyncService;
use email::outbound::mailbox_pg::PgMailboxSync;
use email_api_client::domain::models::{EmailApiError, MailboxAccess, SendRequest};
use email_api_client::domain::ports::AlwaysAllowRateLimiter;
use email_api_client::outbound::imap::{ImapMailboxRepository, MailServers, Security, Server};
use macro_authorization::{InternalIdentityClaims, MacroAuthorizationError};
use macro_db_migrator::MACRO_DB_MIGRATIONS;
use model_user::UserContext;
use models_email::service::address::ContactInfo;
use models_email::service::link::UserProvider;
use models_email::service::message::MessageToSend;
use rootcause::Report;
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tower::ServiceExt;

/// The tests here set the stack's iCloud login, which is process-wide.
static LOGIN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn set_login(apple_id: &str, password: &str) {
    // SAFETY: every test that reads or writes these variables holds `LOGIN`.
    unsafe {
        std::env::set_var("ICLOUD_APPLE_ID", apple_id);
        std::env::set_var("ICLOUD_APP_PASSWORD", password);
    }
}

/// Answers a bearer token with the Macro user it names.
#[derive(Clone)]
struct FakeAuthorization;

impl MacroAuthorizationService for FakeAuthorization {
    async fn authorize(&self, jwt: &str) -> Result<UserContext, Report<MacroAuthorizationError>> {
        Ok(UserContext {
            user_id: jwt.to_string(),
            fusion_user_id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(),
            permissions: None,
            organization_id: None,
        })
    }

    async fn authorize_identity(
        &self,
        jwt: &str,
    ) -> Result<macro_authorization::ValidatedCredential, Report<MacroAuthorizationError>> {
        self.authorize(jwt)
            .await
            .map(macro_authorization::ValidatedCredential::session)
    }

    async fn authorize_internal(
        &self,
        _provided_key: &str,
        _claims: InternalIdentityClaims,
    ) -> Result<Option<UserContext>, Report<MacroAuthorizationError>> {
        Err(Report::new(MacroAuthorizationError::InvalidCredentials))
    }
}

#[derive(Clone)]
struct RouteState {
    links: Arc<ICloudLinks>,
    authorization: MacroAuthorizationState<FakeAuthorization>,
}

impl FromRef<RouteState> for Arc<ICloudLinks> {
    fn from_ref(state: &RouteState) -> Self {
        state.links.clone()
    }
}

impl FromRef<RouteState> for MacroAuthorizationState<FakeAuthorization> {
    fn from_ref(state: &RouteState) -> Self {
        state.authorization.clone()
    }
}

/// The routes as `/email/icloud` serves them, over `links`.
struct Route(Router);

impl Route {
    fn new(links: ICloudLinks) -> Self {
        Self(
            router::<RouteState, FakeAuthorization>().with_state(RouteState {
                links: Arc::new(links),
                authorization: MacroAuthorizationState::new(Arc::new(FakeAuthorization)),
            }),
        )
    }

    async fn call(&self, method: &str, user: &str) -> (StatusCode, serde_json::Value) {
        let response = self
            .0
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .method(method)
                    .header("authorization", format!("Bearer {user}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap_or_default())
    }

    async fn available(&self, user: &str) -> bool {
        let (status, body) = self.call("GET", user).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["available"].as_bool().expect("available")
    }
}

/// The row `deploy/aesv/connect-icloud.sql` writes: the owner, mail sync off.
async fn script_link(db: &PgPool, owner: &str, address: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO email_links (id,macro_id,fusionauth_user_id,email_address,provider,is_sync_active) VALUES ($1,$2,'owner-id',$3,'ICLOUD',false)",
        id,
        owner,
        address
    )
    .execute(db)
    .await
    .expect("scripted link");
    id
}

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

#[sqlx::test(migrator = "MACRO_DB_MIGRATIONS")]
async fn only_the_scripted_owner_can_link_through_the_route(db: PgPool) {
    let _login = LOGIN.lock().await;
    let address = format!("{}@icloud.test", Uuid::new_v4().simple());
    set_login(&address, "pass");
    let owner = format!("macro|{address}");
    let other = "macro|someone@else.test";
    // Every refusal lands before a login, so no mail server is needed.
    let route = Route::new(ICloudLinks {
        db: db.clone(),
        repository: test_servers(),
    });

    // No link row: nobody may link the stack's mailbox, the owner included.
    assert!(!route.available(&owner).await);
    assert_eq!(route.call("POST", &owner).await.0, StatusCode::NOT_FOUND);
    assert_eq!(route.call("POST", other).await.0, StatusCode::NOT_FOUND);

    // The script names the owner: anyone else is refused and writes nothing.
    script_link(&db, &owner, &address).await;
    assert!(route.available(&owner).await);
    assert!(!route.available(other).await);
    assert_eq!(route.call("POST", other).await.0, StatusCode::CONFLICT);
    let rows =
        sqlx::query!("SELECT macro_id,is_sync_active FROM email_links WHERE provider = 'ICLOUD'")
            .fetch_all(&db)
            .await
            .expect("links");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].macro_id, owner);
    assert!(!rows[0].is_sync_active);
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

async fn needs_reauth(db: &PgPool, link_id: Uuid) -> bool {
    sqlx::query_scalar!(
        "SELECT needs_reauth FROM email_links WHERE id = $1",
        link_id
    )
    .fetch_one(db)
    .await
    .expect("link")
}

#[sqlx::test(migrator = "MACRO_DB_MIGRATIONS")]
#[ignore = "needs the IMAP and SMTP test servers: just icloud-mail-test-servers"]
async fn icloud_account_links_syncs_its_inbox_and_sends(db: PgPool) {
    let _login = LOGIN.lock().await;
    let address = format!("{}@icloud.test", Uuid::new_v4().simple());
    set_login(&address, "pass");
    let repository = test_servers();
    deliver(
        &address,
        &format!("From: sam@example.com\r\nTo: {address}\r\nSubject: hello from sam\r\nMessage-ID: <hello-{address}>\r\n\r\nHi there.\r\n"),
    )
    .await;

    // Link through the route: mail sync turns the scripted row on.
    let owner = format!("macro|{address}");
    let scripted = script_link(&db, &owner, &address).await;
    let route = Route::new(ICloudLinks {
        db: db.clone(),
        repository: repository.clone(),
    });
    let (status, body) = route.call("POST", &owner).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let link_id: Uuid = serde_json::from_value(body["link_id"].clone()).expect("link_id");
    assert_eq!(link_id, scripted);
    let (status, again) = route.call("POST", &owner).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["link_id"], body["link_id"]);
    assert_eq!(
        route.call("POST", "macro|someone@else.test").await.0,
        StatusCode::CONFLICT
    );

    // Sync in: the inbox's message lands as a message of the link.
    let pg = PgMailboxSync::new(db.clone()).for_provider(UserProvider::ICloud);
    let mailbox = crate::outbound::email_api::icloud_mailbox(repository.clone(), db.clone());
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
        ICloudEnvCredential::default(),
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

    // A revoked app password marks the link, rather than retrying unseen.
    set_login(&address, "revoked");
    let generations = sqlx::query!(
        "SELECT grant_generation,sync_generation FROM email_links WHERE id = $1",
        link_id
    )
    .fetch_one(&db)
    .await
    .expect("generations");
    // A new adapter, so no session logged in with the old password is reused.
    let revoked = crate::outbound::email_api::icloud_mailbox(test_servers(), db.clone());
    let refused = revoked
        .folders(MailboxAccess {
            link_id,
            grant_generation: generations.grant_generation,
            sync_generation: generations.sync_generation,
        })
        .await;
    assert!(
        matches!(refused, Err(EmailApiError::AuthRequired)),
        "{refused:?}"
    );
    assert!(needs_reauth(&db, link_id).await);

    // Linking again with the right password clears it.
    set_login(&address, "pass");
    assert_eq!(route.call("POST", &owner).await.0, StatusCode::OK);
    assert!(!needs_reauth(&db, link_id).await);
}
