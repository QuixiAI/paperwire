//! The M2 milestone (design.md §27 Phase 0): a client can use only the
//! capabilities granted to it, and a revocation applies at once.
//!
//! State is set up through the control-plane library, as the user would
//! with `postal-mcp admin`; the client side is the real binary over stdio.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};

use postal_mcp::admin::{Admin, AdminError, GrantRequest, Terminal};
use postal_mcp::connections::{Connection, MemoryStore};
use postal_mcp::core::{Capability, Money, Secret, clock};
use postal_mcp::delegation::{Constraints, Grant};
use postal_mcp::providers::{Environment, Registry};
use postal_mcp::store::Store;
use serde_json::{Value, json};

use common::Server;

/// A user at the terminal who confirms everything.
struct Confirms;

impl Terminal for Confirms {
    fn confirm(&mut self, _: &str) -> Result<bool, AdminError> {
        Ok(true)
    }

    fn read_secret(&mut self, _: &str) -> Result<Secret, AdminError> {
        unreachable!("the mock provider takes no credential")
    }
}

struct ControlPlane {
    store: Store,
    providers: Registry,
    secrets: MemoryStore,
}

impl ControlPlane {
    fn open(data_dir: &Path) -> Self {
        Self {
            store: Store::open(data_dir).unwrap(),
            providers: Registry::builtin().unwrap(),
            secrets: MemoryStore::default(),
        }
    }

    fn admin<'a>(&'a self, terminal: &'a mut Confirms) -> Admin<'a> {
        Admin {
            store: &self.store,
            providers: &self.providers,
            secrets: &self.secrets,
            terminal,
        }
    }

    async fn connect_mock(&self) -> Connection {
        self.admin(&mut Confirms)
            .connect("mock", Environment::Test, None)
            .await
            .unwrap()
    }

    /// The MVP grant: domestic letters under $3, $30 a month.
    fn grant_letters(&self, client: &str, connection: &Connection) -> Grant {
        self.admin(&mut Confirms)
            .grant(GrantRequest {
                principal_id: client.into(),
                connection_ids: vec![connection.id.clone()],
                actions: vec![Capability::PostalSendLetter, Capability::PostalReadStatus],
                constraints: Constraints {
                    destination_countries: Some(vec!["US".into()]),
                    max_cost_per_item: Some(Money::new("USD", 300)),
                    monthly_budget: Some(Money::new("USD", 3000)),
                    ..Constraints::default()
                },
                expires_at: None,
            })
            .unwrap()
    }
}

fn letter(cost_minor: i64) -> Value {
    json!({
        "destination_country": "US",
        "page_count": 2,
        "max_cost": {"currency": "USD", "amount_minor": cost_minor}
    })
}

fn check(server: &mut Server, connection_id: &str, action: &str, context: Value) -> Value {
    server.call_ok(
        "permissions_check",
        json!({"connection_id": connection_id, "action": action, "context": context}),
    )
}

#[tokio::test]
async fn a_client_can_use_only_what_it_was_granted() {
    let dir = tempfile::tempdir().unwrap();
    let control = ControlPlane::open(dir.path());
    let connection = control.connect_mock().await;
    let conn = connection.id.as_str();
    let mut assistant = Server::start(dir.path(), "assistant");

    // Connected but nothing granted: default deny, and the connection is
    // not even visible.
    let listed = assistant.call_ok("connections_list", json!({}));
    assert_eq!(listed["connections"], json!([]));
    let denied = check(&mut assistant, conn, "postal.send.letter", letter(250));
    assert_eq!(denied["allowed"], false);
    assert_eq!(denied["reason"], "ACTION_NOT_DELEGATED");

    let grant = control.grant_letters("assistant", &connection);

    // The grant is visible to its client, limits included.
    let listed = assistant.call_ok("connections_list", json!({}));
    assert_eq!(listed["connections"][0]["connection_id"], conn);
    let status = assistant.call_ok("connections_status", json!({"connection_id": conn}));
    assert_eq!(status["status"], "connected");
    let permissions = assistant.call_ok("permissions_get", json!({}));
    assert_eq!(permissions["client"], "assistant");
    let granted = &permissions["grants"][0];
    assert_eq!(granted["grant_id"], grant.id.as_str());
    assert_eq!(
        granted["actions"],
        json!(["postal.send.letter", "postal.read.status"])
    );
    assert_eq!(
        granted["constraints"]["max_cost_per_item"],
        json!({"currency": "USD", "amount_minor": 300})
    );

    // Within the grant.
    let allowed = check(&mut assistant, conn, "postal.send.letter", letter(250));
    assert_eq!(allowed["allowed"], true, "{allowed}");
    assert_eq!(allowed["decision"], "allow");
    assert_eq!(allowed["grant_id"], grant.id.as_str());
    let read = check(&mut assistant, conn, "postal.read.status", json!({}));
    assert_eq!(read["allowed"], true);

    // Outside it.
    let cases = [
        ("postal.send.letter", letter(301), "COST_LIMIT_EXCEEDED"),
        ("fax.send", letter(250), "ACTION_NOT_DELEGATED"),
        ("postal.send.letter", json!({}), "CONTEXT_REQUIRED"),
        (
            "postal.send.letter",
            json!({"destination_country": "CA", "max_cost": {"currency": "USD", "amount_minor": 250}}),
            "DESTINATION_NOT_ALLOWED",
        ),
    ];
    for (action, context, reason) in cases {
        let result = check(&mut assistant, conn, action, context);
        assert_eq!(result["allowed"], false, "{action}: {result}");
        assert_eq!(result["reason"], reason, "{action}: {result}");
    }
    let unknown = check(
        &mut assistant,
        "conn_nope",
        "postal.send.letter",
        letter(250),
    );
    assert_eq!(unknown["reason"], "ACTION_NOT_DELEGATED");

    // Spend already recorded against the grant counts toward the budget.
    control
        .store
        .record_usage(
            &grant.id,
            "op_1",
            Some(&Money::new("USD", 2900)),
            1,
            clock::now(),
        )
        .unwrap();
    let over = check(&mut assistant, conn, "postal.send.letter", letter(250));
    assert_eq!(over["reason"], "BUDGET_EXCEEDED");
    let fits = check(&mut assistant, conn, "postal.send.letter", letter(100));
    assert_eq!(fits["allowed"], true);

    // Another client on the same machine has nothing.
    let mut other = Server::start(dir.path(), "other");
    let listed = other.call_ok("connections_list", json!({}));
    assert_eq!(listed["connections"], json!([]));
    assert_eq!(
        other.call_ok("permissions_get", json!({}))["grants"],
        json!([])
    );
    let hidden = other.call("connections_status", json!({"connection_id": conn}));
    assert_eq!(hidden["isError"], true);
    let denied = check(&mut other, conn, "postal.send.letter", letter(100));
    assert_eq!(denied["reason"], "ACTION_NOT_DELEGATED");

    // Revocation applies to the running server on its next call.
    control.admin(&mut Confirms).revoke(&grant.id).unwrap();
    let revoked = check(&mut assistant, conn, "postal.send.letter", letter(100));
    assert_eq!(revoked["allowed"], false);
    assert_eq!(revoked["reason"], "ACTION_NOT_DELEGATED");
    assert_eq!(
        assistant.call_ok("permissions_get", json!({}))["grants"],
        json!([])
    );

    // Every decision was recorded.
    let events = control.store.audit_events(100).unwrap();
    let denials = events
        .iter()
        .filter(|r| r.event.kind == "authorization.checked")
        .filter(|r| r.event.decision.as_deref() == Some("deny"))
        .count();
    assert_eq!(denials, 9);
    let newest = &events[0].event;
    assert_eq!(newest.principal_id.as_deref(), Some("assistant"));
    assert_eq!(newest.reason.as_deref(), Some("ACTION_NOT_DELEGATED"));
}

#[tokio::test]
async fn disconnecting_an_account_stops_its_grants() {
    let dir = tempfile::tempdir().unwrap();
    let control = ControlPlane::open(dir.path());
    let connection = control.connect_mock().await;
    control.grant_letters("assistant", &connection);
    let mut assistant = Server::start(dir.path(), "assistant");

    let before = check(
        &mut assistant,
        &connection.id,
        "postal.send.letter",
        letter(100),
    );
    assert_eq!(before["allowed"], true);

    control
        .admin(&mut Confirms)
        .disconnect(&connection.id)
        .unwrap();
    let after = check(
        &mut assistant,
        &connection.id,
        "postal.send.letter",
        letter(100),
    );
    assert_eq!(after["allowed"], false);
    assert_eq!(after["reason"], "CONNECTION_NOT_READY");
}

fn admin(data_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_postal-mcp"))
        .arg("--data-dir")
        .arg(data_dir)
        .arg("admin")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// What an AI with shell access gets when it runs the admin CLI itself:
/// there is no terminal, so nothing that widens its authority goes through.
#[tokio::test]
async fn admin_cli_will_not_connect_or_grant_without_a_terminal() {
    let dir = tempfile::tempdir().unwrap();

    let connect = admin(dir.path(), &["connect", "mock"]);
    assert!(!connect.status.success());
    let stderr = String::from_utf8_lossy(&connect.stderr);
    assert!(stderr.contains("interactive terminal"), "{stderr}");

    let control = ControlPlane::open(dir.path());
    assert!(control.store.connections().unwrap().is_empty());
    let connection = control.connect_mock().await;

    let grant = admin(
        dir.path(),
        &[
            "grant",
            "--client",
            "assistant",
            "--connection",
            &connection.id,
            "--action",
            "postal.send.letter",
        ],
    );
    assert!(!grant.status.success());
    let stderr = String::from_utf8_lossy(&grant.stderr);
    assert!(stderr.contains("interactive terminal"), "{stderr}");
    assert!(control.store.grants(None).unwrap().is_empty());

    // Reading and narrowing still work without one.
    let list = admin(dir.path(), &["connections"]);
    assert!(list.status.success());
    assert!(String::from_utf8_lossy(&list.stdout).contains(&connection.id));
    let granted = control.grant_letters("assistant", &connection);
    let revoke = admin(dir.path(), &["revoke", &granted.id]);
    assert!(revoke.status.success(), "{revoke:?}");
    let stored = control.store.grant(&granted.id).unwrap().unwrap();
    assert!(stored.revoked_at.is_some());
}

/// The MVP demonstration of design.md §29, on the mock provider: grant
/// "domestic letters under $3", send one, read its status, revoke, and see
/// the next send denied.
#[tokio::test]
async fn send_a_letter_then_revoke() {
    let dir = tempfile::tempdir().unwrap();
    let control = ControlPlane::open(dir.path());
    let connection = control.connect_mock().await;
    let grant = control.grant_letters("assistant", &connection);
    let mut assistant = Server::start(dir.path(), "assistant");

    let document = assistant.call_ok(
        "documents_prepare",
        json!({"content": "Dear Jane,\n\nPlease find my reply enclosed.\n", "title": "Reply"}),
    );
    assert_eq!(document["page_count"], 1);
    assert_eq!(document["page_count_estimated"], true);

    let quote_request = json!({
        "connection_id": connection.id,
        "product": "letter",
        "to": {
            "name": "Jane Doe",
            "line1": "1 Main St",
            "city": "Springfield",
            "state": "IL",
            "postal_code": "62701",
            "country": "us"
        },
        "document_ids": [document["document_id"]]
    });
    let quote = assistant.call_ok("correspondence_quote", quote_request.clone());
    assert_eq!(quote["price_kind"], "firm");
    assert_eq!(
        quote["price"],
        json!({"currency": "USD", "amount_minor": 100})
    );
    assert_eq!(quote["authorization"]["allowed"], true, "{quote}");

    let sent = assistant.call_ok(
        "correspondence_send",
        json!({"quote_id": quote["quote_id"]}),
    );
    assert_eq!(sent["accepted"], true, "{sent}");
    let operation = &sent["operation"];
    assert_eq!(operation["status"], "accepted");
    assert!(operation["provider_reference"].is_string());

    // Sending the same quote again mails nothing new.
    let again = assistant.call_ok(
        "correspondence_send",
        json!({"quote_id": quote["quote_id"]}),
    );
    assert_eq!(
        again["operation"]["operation_id"],
        operation["operation_id"]
    );

    let status = assistant.call_ok(
        "correspondence_status",
        json!({"operation_id": operation["operation_id"]}),
    );
    assert_eq!(status["status"], "accepted");
    assert!(status["note"].as_str().unwrap().contains("not proof"));
    let usage = control
        .store
        .usage(&grant.id, Some("USD"), clock::now())
        .unwrap();
    assert_eq!(usage.spent_this_month_minor, 100);

    // Another client cannot read the operation.
    let mut other = Server::start(dir.path(), "other");
    let hidden = other.call(
        "correspondence_status",
        json!({"operation_id": operation["operation_id"]}),
    );
    assert_eq!(hidden["isError"], true);

    // A quote taken while permitted is still refused after revocation.
    let second = assistant.call_ok("correspondence_quote", quote_request.clone());
    control.admin(&mut Confirms).revoke(&grant.id).unwrap();
    let denied = assistant.call_ok(
        "correspondence_send",
        json!({"quote_id": second["quote_id"]}),
    );
    assert_eq!(denied["accepted"], false);
    assert_eq!(denied["operation"], Value::Null);
    assert_eq!(denied["authorization"]["reason"], "ACTION_NOT_DELEGATED");
    // With no grant left, the connection is no longer visible at all.
    let refused = assistant.call("correspondence_quote", quote_request);
    assert_eq!(refused["isError"], true);

    assert_eq!(control.store.operations(None).unwrap().len(), 1);
    let kinds: Vec<String> = control
        .store
        .audit_events(100)
        .unwrap()
        .into_iter()
        .map(|r| r.event.kind)
        .collect();
    for kind in [
        "grant.created",
        "authorization.enforced",
        "operation.accepted",
        "grant.revoked",
    ] {
        assert!(
            kinds.contains(&kind.to_owned()),
            "{kind} missing from {kinds:?}"
        );
    }
}

/// A PDF is imported by path, but only from a directory the user allowed.
#[tokio::test]
async fn pdfs_are_read_only_from_allowed_directories() {
    let dir = tempfile::tempdir().unwrap();
    let control = ControlPlane::open(dir.path());
    let connection = control.connect_mock().await;
    control.grant_letters("assistant", &connection);

    let allowed = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let inside = allowed.path().join("reply.pdf");
    let outside = elsewhere.path().join("private.pdf");
    std::fs::write(&inside, common::sample_pdf(2)).unwrap();
    std::fs::write(&outside, common::sample_pdf(1)).unwrap();

    let mut assistant = Server::start_with(
        dir.path(),
        "assistant",
        &["--document-dir", allowed.path().to_str().unwrap()],
    );
    let document = assistant.call_ok("documents_prepare", json!({"path": inside}));
    assert_eq!(document["format"], "pdf");
    assert_eq!(document["page_count"], 2);
    assert_eq!(document["page_count_estimated"], false);

    let refused = assistant.call("documents_prepare", json!({"path": outside}));
    assert_eq!(refused["isError"], true);
    assert!(
        refused["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("--document-dir")
    );
    let both = assistant.call("documents_prepare", json!({"path": inside, "content": "x"}));
    assert_eq!(both["isError"], true);

    // The PDF's real page count is what gets priced: 80 + 2 * 20.
    let quote = assistant.call_ok(
        "correspondence_quote",
        json!({
            "connection_id": connection.id,
            "product": "letter",
            "to": {
                "name": "Jane Doe", "line1": "1 Main St", "city": "Springfield",
                "state": "IL", "postal_code": "62701", "country": "US"
            },
            "document_ids": [document["document_id"]]
        }),
    );
    assert_eq!(quote["page_count"], 2);
    assert_eq!(quote["price"]["amount_minor"], 120);
    let sent = assistant.call_ok(
        "correspondence_send",
        json!({"quote_id": quote["quote_id"]}),
    );
    assert_eq!(sent["accepted"], true, "{sent}");

    // A server started without the option reads no file at all.
    let mut plain = Server::start(dir.path(), "assistant");
    let refused = plain.call("documents_prepare", json!({"path": inside}));
    assert_eq!(refused["isError"], true);
}
