//! The Stable adapter and the inbound read path against a local stand-in
//! for api.usestable.com. Requests and responses follow Stable's published
//! API; none of this has run against Stable itself.

use std::sync::Arc;
use std::time::Duration;

use paperwire::connections::{
    Connection, ConnectionStatus, CredentialRef, MemoryStore, SecretStore,
};
use paperwire::core::{Capability, Secret, clock};
use paperwire::delegation::{Constraints, Grant, Reason};
use paperwire::http::{ReqwestTransport, RetryPolicy, Retrying};
use paperwire::inbound::{EndpointStatus, Inbound, InboundError, InboxRequest, ScanStatus};
use paperwire::providers::{
    Account, Environment, ProviderAdapter, ProviderError, Registry, StableAdapter,
};
use paperwire::store::Store;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT: &str = "assistant";
const CONN: &str = "conn_stable_1";
const GRANT: &str = "grant_1";
const KEY: &str = "stbl_dev_key";

const ALL_READS: [Capability; 4] = [
    Capability::MailboxReadItems,
    Capability::MailboxReadEnvelopes,
    Capability::MailboxReadScans,
    Capability::EndpointRead,
];

type Stable = StableAdapter<Retrying<ReqwestTransport>>;

fn adapter(server: &MockServer) -> Stable {
    let policy = RetryPolicy {
        max_attempts: 3,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(20),
    };
    StableAdapter::with_base_url(
        Retrying::new(ReqwestTransport::new().unwrap(), policy),
        server.uri(),
    )
}

struct World {
    server: MockServer,
    store: Store,
    inbound: Inbound,
}

/// A connected Stable sandbox account and a grant of `actions` on it.
async fn world(actions: &[Capability]) -> World {
    let server = MockServer::start().await;
    let store = Store::open_in_memory().unwrap();
    let secrets = MemoryStore::default();
    secrets.set(CONN, &Secret::new(KEY)).unwrap();
    store
        .insert_connection(&Connection {
            id: CONN.into(),
            provider: "stable".into(),
            environment: Environment::Test,
            status: ConnectionStatus::Connected,
            capabilities: ALL_READS.to_vec(),
            credential: CredentialRef::Keyring,
            created_at: clock::now(),
        })
        .unwrap();
    store
        .insert_grant(&Grant {
            id: GRANT.into(),
            principal_id: CLIENT.into(),
            connection_ids: vec![CONN.into()],
            actions: actions.to_vec(),
            constraints: Constraints::default(),
            policy_version: 1,
            created_at: clock::now(),
            expires_at: None,
            revoked_at: None,
        })
        .unwrap();
    let inbound = Inbound::new(
        store.clone(),
        Registry::new(vec![Arc::new(adapter(&server))]),
        Arc::new(secrets),
    );
    World {
        server,
        store,
        inbound,
    }
}

fn location() -> Value {
    json!({
        "id": "loc_1",
        "status": "active",
        "type": "cmra",
        "onboarding": {"status": "complete"},
        "metadata": {},
        "address": {
            "line1": "2261 Market Street",
            "line2": "#4242",
            "city": "San Francisco",
            "state": "CA",
            "postalCode": "94114"
        }
    })
}

/// A mail item in the shape of Stable's reference.
fn mail_item(id: &str, scan: Value) -> Value {
    json!({
        "id": id,
        "from": "Internal Revenue Service",
        "recipients": {"line1": {"text": "Acme Robotics LLC"}},
        "createdAt": "2026-01-05T15:00:00Z",
        "imageUrl": format!("https://files.example/{id}/envelope?sig=abc"),
        "location": location(),
        "scanDetails": scan,
        "isReturnedToSender": false,
        "clearAt": "2026-03-05T15:00:00Z",
        "checks": [],
        "tags": [],
        "teams": []
    })
}

fn scanned(id: &str) -> Value {
    mail_item(
        id,
        json!({
            "status": "completed",
            "imageUrl": format!("https://files.example/{id}/scan.pdf?sig=abc"),
            "summary": "Notice CP2000: a proposed change to the 2024 return."
        }),
    )
}

fn list(nodes: Vec<Value>, next: Option<&str>) -> Value {
    json!({
        "edges": nodes.into_iter().map(|node| json!({"cursor": "c", "node": node})).collect::<Vec<_>>(),
        "pageInfo": {
            "hasNextPage": next.is_some(),
            "hasPreviousPage": false,
            "endCursor": next,
        },
        "totalCount": 3
    })
}

fn request(connection_id: &str) -> InboxRequest {
    InboxRequest {
        connection_id: connection_id.into(),
        ..InboxRequest::default()
    }
}

async fn requests(server: &MockServer) -> usize {
    server.received_requests().await.unwrap().len()
}

#[tokio::test]
async fn verify_sends_the_key_and_reports_read_capabilities() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/locations"))
        .and(query_param("first", "1"))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(vec![location()], None)))
        .expect(1)
        .mount(&server)
        .await;

    let capabilities = adapter(&server)
        .verify(Environment::Test, Some(&Secret::new(KEY)))
        .await
        .unwrap();
    assert_eq!(capabilities, ALL_READS);
    // Stable only reads: nothing here can send, scan, forward or shred.
    assert!(capabilities.iter().all(|c| c.is_read()));
}

#[tokio::test]
async fn a_rejected_key_is_unauthorized() {
    let server = MockServer::start().await;
    // Stable answers a bad key with 400, not 401.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "statusCode": 400,
            "code": "INVALID_API_KEY",
            "message": "Invalid API key."
        })))
        .mount(&server)
        .await;

    let result = adapter(&server)
        .verify(Environment::Test, Some(&Secret::new(KEY)))
        .await;
    assert!(matches!(result, Err(ProviderError::Unauthorized)));
}

#[tokio::test]
async fn a_key_is_never_sent_to_the_other_environment() {
    let server = MockServer::start().await;
    let stable = adapter(&server);

    for (environment, key) in [
        (Environment::Live, "stbl_dev_key"),
        (Environment::Test, "production_key"),
    ] {
        let result = stable.verify(environment, Some(&Secret::new(key))).await;
        assert!(matches!(result, Err(ProviderError::Other(_))), "{key}");
    }
    let no_key = stable.verify(Environment::Test, None).await;
    assert!(matches!(no_key, Err(ProviderError::CredentialRequired)));
    assert_eq!(requests(&server).await, 0);

    assert_eq!(
        stable.credential_environment(&Secret::new("stbl_dev_key")),
        Some(Environment::Test)
    );
    assert_eq!(
        stable.credential_environment(&Secret::new("production_key")),
        None
    );
}

#[tokio::test]
async fn endpoints_are_the_mailbox_addresses() {
    let w = world(&ALL_READS).await;
    let mut unfinished = location();
    unfinished["id"] = json!("loc_2");
    unfinished["onboarding"]["status"] = json!("verify");
    Mock::given(method("GET"))
        .and(path("/v1/locations"))
        .and(header("x-api-key", KEY))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(list(vec![location(), unfinished], None)),
        )
        .mount(&w.server)
        .await;

    let endpoints = w.inbound.endpoints(CLIENT, CONN).await.unwrap();
    assert_eq!(endpoints.len(), 2);
    let address = endpoints[0].address.as_ref().unwrap();
    assert_eq!(endpoints[0].endpoint_id, "loc_1");
    assert_eq!(endpoints[0].status, EndpointStatus::Active);
    assert_eq!(address.line1, "2261 Market Street");
    assert_eq!(address.line2.as_deref(), Some("#4242"));
    assert_eq!(address.postal_code, "94114");
    assert_eq!(address.country, "US");
    // A mailbox whose paperwork is unfinished does not receive mail yet.
    assert_eq!(endpoints[1].status, EndpointStatus::Inactive);
    assert!(endpoints[1].detail.as_ref().unwrap().contains("verify"));
}

#[tokio::test]
async fn the_inbox_lists_mail_without_images_or_contents() {
    let w = world(&ALL_READS).await;
    Mock::given(method("GET"))
        .and(path("/v1/mail-items"))
        .and(query_param("first", "25"))
        .and(query_param_is_missing("locationId"))
        .and(query_param_is_missing("after"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(
            vec![
                scanned("mi_2"),
                mail_item("mi_1", json!({"status": null, "imageUrl": ""})),
            ],
            Some("cursor_2"),
        )))
        .mount(&w.server)
        .await;

    let page = w.inbound.inbox(CLIENT, request(CONN)).await.unwrap();
    assert_eq!(page.next_cursor.as_deref(), Some("cursor_2"));
    let [first, second] = &page.items[..] else {
        panic!("two items expected");
    };
    assert_eq!(first.item_id, "mi_2");
    assert_eq!(first.sender.as_deref(), Some("Internal Revenue Service"));
    assert_eq!(first.recipient.as_deref(), Some("Acme Robotics LLC"));
    assert_eq!(first.received_at.as_deref(), Some("2026-01-05T15:00:00Z"));
    assert_eq!(first.endpoint_id.as_deref(), Some("loc_1"));
    assert_eq!(first.scan_status, ScanStatus::Completed);
    assert_eq!(second.scan_status, ScanStatus::None);
    // Even with every permission, a listing carries no links or contents.
    for item in &page.items {
        assert_eq!(item.envelope_image_url, None);
        assert_eq!(item.scan, None);
    }
}

#[tokio::test]
async fn inbox_filters_and_cursor_reach_stable_encoded() {
    let w = world(&ALL_READS).await;
    Mock::given(method("GET"))
        .and(path("/v1/mail-items"))
        .and(query_param("first", "5"))
        .and(query_param("locationId", "loc_1"))
        .and(query_param("createdAt_gte", "2026-01-01T00:00:00Z"))
        .and(query_param("after", "a b&first=500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(vec![], None)))
        .expect(1)
        .mount(&w.server)
        .await;

    let page = w
        .inbound
        .inbox(
            CLIENT,
            InboxRequest {
                connection_id: CONN.into(),
                endpoint_id: Some("loc_1".into()),
                // An offset is normalized to UTC.
                received_after: Some("2026-01-01T02:00:00+02:00".into()),
                limit: Some(5),
                // A cursor cannot add or change a query parameter.
                cursor: Some("a b&first=500".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items, vec![]);
    assert_eq!(page.next_cursor, None);
}

#[tokio::test]
async fn bad_inbox_requests_never_reach_stable() {
    let w = world(&ALL_READS).await;
    let bad = [
        InboxRequest {
            endpoint_id: Some("loc_1&archived=true".into()),
            ..request(CONN)
        },
        InboxRequest {
            received_after: Some("last week".into()),
            ..request(CONN)
        },
        InboxRequest {
            limit: Some(0),
            ..request(CONN)
        },
        InboxRequest {
            limit: Some(101),
            ..request(CONN)
        },
        InboxRequest {
            cursor: Some("x".repeat(2000)),
            ..request(CONN)
        },
    ];
    for request in bad {
        let result = w.inbound.inbox(CLIENT, request.clone()).await;
        assert!(
            matches!(result, Err(InboundError::Invalid(_))),
            "{request:?}"
        );
    }
    for item_id in ["../locations", "mi_1/scan", "mi_1?x=1", ""] {
        let result = w.inbound.read(CLIENT, CONN, item_id).await;
        assert!(matches!(result, Err(InboundError::Invalid(_))), "{item_id}");
    }
    assert_eq!(requests(&w.server).await, 0);
}

#[tokio::test]
async fn reading_an_item_shows_only_the_parts_delegated() {
    // Items only: the client learns that mail arrived, not what it says.
    let w = world(&[Capability::MailboxReadItems]).await;
    Mock::given(method("GET"))
        .and(path("/v1/mail-items/mi_2"))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(scanned("mi_2")))
        .mount(&w.server)
        .await;

    let read = w.inbound.read(CLIENT, CONN, "mi_2").await.unwrap();
    assert_eq!(
        read.item.sender.as_deref(),
        Some("Internal Revenue Service")
    );
    assert_eq!(read.item.scan_status, ScanStatus::Completed);
    assert_eq!(read.item.envelope_image_url, None);
    assert_eq!(read.item.scan, None);
    assert_eq!(
        read.withheld,
        [
            Capability::MailboxReadEnvelopes,
            Capability::MailboxReadScans
        ]
    );

    // With the scan permission added, the contents appear; the envelope
    // image is still withheld.
    w.store
        .insert_grant(&Grant {
            id: "grant_2".into(),
            principal_id: CLIENT.into(),
            connection_ids: vec![CONN.into()],
            actions: vec![Capability::MailboxReadScans],
            constraints: Constraints::default(),
            policy_version: 1,
            created_at: clock::now(),
            expires_at: None,
            revoked_at: None,
        })
        .unwrap();
    let read = w.inbound.read(CLIENT, CONN, "mi_2").await.unwrap();
    let scan = read.item.scan.unwrap();
    assert_eq!(
        scan.summary.as_deref(),
        Some("Notice CP2000: a proposed change to the 2024 return.")
    );
    assert_eq!(
        scan.document_url.as_deref(),
        Some("https://files.example/mi_2/scan.pdf?sig=abc")
    );
    assert_eq!(read.item.envelope_image_url, None);
    assert_eq!(read.withheld, [Capability::MailboxReadEnvelopes]);
}

#[tokio::test]
async fn an_unscanned_item_has_an_envelope_and_no_contents() {
    let w = world(&ALL_READS).await;
    Mock::given(method("GET"))
        .and(path("/v1/mail-items/mi_1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mail_item(
            "mi_1",
            json!({"status": "processing", "imageUrl": "https://files.example/partial"}),
        )))
        .mount(&w.server)
        .await;

    let read = w.inbound.read(CLIENT, CONN, "mi_1").await.unwrap();
    assert_eq!(read.item.scan_status, ScanStatus::Processing);
    assert_eq!(read.item.scan, None);
    assert_eq!(
        read.item.envelope_image_url.as_deref(),
        Some("https://files.example/mi_1/envelope?sig=abc")
    );
    assert_eq!(read.withheld, []);
}

#[tokio::test]
async fn a_missing_item_and_a_stable_error_are_reported_plainly() {
    let w = world(&ALL_READS).await;
    Mock::given(method("GET"))
        .and(path("/v1/mail-items/mi_404"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "statusCode": 404,
            "code": "MAIL_ITEM_NOT_FOUND",
            "message": "This mail item could not be found."
        })))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/mail-items"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "statusCode": 500,
            "code": "INTERNAL_ERROR",
            "message": "Something went wrong.",
            "debug": "do not echo this"
        })))
        .mount(&w.server)
        .await;

    let missing = w.inbound.read(CLIENT, CONN, "mi_404").await;
    assert!(matches!(missing, Err(InboundError::Invalid(m)) if m.contains("mi_404")));

    let Err(InboundError::Provider(message)) = w.inbound.inbox(CLIENT, request(CONN)).await else {
        panic!("a provider error expected");
    };
    assert_eq!(message, "Stable (500): Something went wrong.");
}

#[tokio::test]
async fn nothing_is_read_without_a_live_grant() {
    let w = world(&[Capability::MailboxReadItems]).await;

    // Endpoints were not delegated, and another client holds nothing.
    let Err(InboundError::Denied(denied)) = w.inbound.endpoints(CLIENT, CONN).await else {
        panic!("a denial expected");
    };
    assert_eq!(denied.reason, Some(Reason::ActionNotDelegated));
    let stranger = w.inbound.inbox("stranger", request(CONN)).await;
    assert!(matches!(stranger, Err(InboundError::Denied(_))));

    // A revocation applies to the next read.
    w.store.revoke_grant(GRANT, clock::now()).unwrap();
    let revoked = w.inbound.inbox(CLIENT, request(CONN)).await;
    assert!(matches!(revoked, Err(InboundError::Denied(_))));
    let revoked = w.inbound.read(CLIENT, CONN, "mi_1").await;
    assert!(matches!(revoked, Err(InboundError::Denied(_))));

    assert_eq!(requests(&w.server).await, 0);
}

#[tokio::test]
async fn stable_cannot_be_used_to_send() {
    let server = MockServer::start().await;
    let stable = adapter(&server);
    let account = Account {
        environment: Environment::Test,
        credential: None,
    };
    assert!(stable.track("anything", account).await.is_err());
    let info = stable.info();
    assert!(!info.capabilities.contains(&Capability::PostalSendLetter));
    assert_eq!(requests(&server).await, 0);
}
