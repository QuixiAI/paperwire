//! The MVP demonstration of design.md §29 against Lob's real test
//! environment. Nothing is printed or mailed and Lob charges nothing for
//! test-key requests, but this does reach api.lob.com, so it is ignored by
//! default. To run it:
//!
//! ```sh
//! LOB_TEST_API_KEY=test_... cargo test --test lob_live -- --ignored
//! ```
//!
//! The key is read from the environment by the server itself (a
//! `--from-env` connection); it is never passed through a tool call.

mod common;

use std::path::Path;

use paperwire::admin::{Admin, AdminError, GrantRequest, Terminal};
use paperwire::connections::MemoryStore;
use paperwire::core::{Capability, Money, Rate, Secret, clock};
use paperwire::delegation::Constraints;
use paperwire::operations::ProviderPrice;
use paperwire::providers::{Environment, Registry};
use paperwire::store::Store;
use serde_json::{Value, json};

use common::Server;

const KEY_VARIABLE: &str = "LOB_TEST_API_KEY";

/// A user at the terminal who confirms everything.
struct Confirms;

impl Terminal for Confirms {
    fn confirm(&mut self, _: &str) -> Result<bool, AdminError> {
        Ok(true)
    }

    fn read_secret(&mut self, _: &str) -> Result<Secret, AdminError> {
        unreachable!("the key comes from the environment")
    }

    fn ask(&mut self, _: &str, _: Option<&str>) -> Result<String, AdminError> {
        unreachable!("nothing here asks questions")
    }

    fn note(&mut self, _: &str) -> Result<(), AdminError> {
        Ok(())
    }
}

/// Refuse to run with anything but a Lob test key: a live key would print
/// and mail real letters.
fn require_test_key() {
    let key = std::env::var(KEY_VARIABLE)
        .unwrap_or_else(|_| panic!("set {KEY_VARIABLE} to a Lob test key (test_...)"));
    assert!(
        key.starts_with("test_"),
        "{KEY_VARIABLE} must be a Lob *test* key (it starts with test_)"
    );
}

fn quote_request(connection_id: &str, document_id: &Value) -> Value {
    json!({
        "connection_id": connection_id,
        "product": "letter",
        "to": {
            "name": "Jane Doe",
            "line1": "210 King St",
            "city": "San Francisco",
            "state": "CA",
            "postal_code": "94107",
            "country": "US"
        },
        "from": {
            "name": "Paperwire Test",
            "line1": "185 Berry St Ste 6100",
            "city": "San Francisco",
            "state": "CA",
            "postal_code": "94107",
            "country": "US"
        },
        "document_ids": [document_id]
    })
}

/// Quote and send one document; returns the accepted operation.
fn send(assistant: &mut Server, connection_id: &str, document: &Value, price_minor: i64) -> Value {
    let quote = assistant.call_ok(
        "correspondence_quote",
        quote_request(connection_id, &document["document_id"]),
    );
    assert_eq!(quote["price_kind"], "estimated", "{quote}");
    assert_eq!(
        quote["price"],
        json!({"currency": "USD", "amount_minor": price_minor}),
        "{quote}"
    );
    assert_eq!(quote["authorization"]["allowed"], true, "{quote}");

    let sent = assistant.call_ok(
        "correspondence_send",
        json!({"quote_id": quote["quote_id"]}),
    );
    assert_eq!(sent["accepted"], true, "{sent}");
    let operation = sent["operation"].clone();
    assert_eq!(operation["status"], "accepted", "{operation}");
    let reference = operation["provider_reference"].as_str().unwrap();
    assert!(reference.starts_with("ltr_"), "{reference}");

    // Sending the same quote again creates nothing new.
    let again = assistant.call_ok(
        "correspondence_send",
        json!({"quote_id": quote["quote_id"]}),
    );
    assert_eq!(
        again["operation"]["provider_reference"],
        operation["provider_reference"]
    );
    operation
}

#[tokio::test]
#[ignore = "reaches api.lob.com; needs LOB_TEST_API_KEY"]
async fn send_letters_through_lob_then_revoke() {
    require_test_key();
    let dir = tempfile::tempdir().unwrap();
    let pdf_dir = tempfile::tempdir().unwrap();
    let pdf_path = pdf_dir.path().join("two-pages.pdf");
    std::fs::write(&pdf_path, common::sample_pdf(2)).unwrap();

    let store = Store::open(dir.path()).unwrap();
    let providers = Registry::builtin().unwrap();
    let secrets = MemoryStore::default();
    let mut terminal = Confirms;
    let mut admin = Admin {
        store: &store,
        providers: &providers,
        secrets: &secrets,
        terminal: &mut terminal,
    };

    // Connecting verifies the key with Lob.
    let connection = admin
        .connect("lob", Environment::Test, Some(KEY_VARIABLE.into()))
        .await
        .expect("Lob should accept the test key");
    assert!(
        connection
            .capabilities
            .contains(&Capability::PostalSendLetter)
    );
    admin
        .set_price(ProviderPrice {
            provider: "lob".into(),
            product: "letter".into(),
            currency: "USD".into(),
            base: Rate::from_minor(120),
            per_extra_page: Rate::from_minor(20),
        })
        .unwrap();
    let grant = admin
        .grant(GrantRequest {
            principal_id: "assistant".into(),
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
        .unwrap();

    let mut assistant = start(dir.path(), pdf_dir.path());

    // A Markdown letter: one page, plus Lob's address page.
    let markdown = assistant.call_ok(
        "documents_prepare",
        json!({
            "content": "# Reply\n\nDear Jane,\n\nPlease find my **reply** enclosed.\n",
            "format": "markdown",
            "title": "Reply"
        }),
    );
    let first = send(&mut assistant, &connection.id, &markdown, 140);

    // A two-page PDF, uploaded as a file.
    let pdf = assistant.call_ok("documents_prepare", json!({"path": pdf_path}));
    assert_eq!(pdf["page_count"], 2, "{pdf}");
    let second = send(&mut assistant, &connection.id, &pdf, 160);
    assert_ne!(first["provider_reference"], second["provider_reference"]);

    // Status includes Lob's own report.
    let status = assistant.call_ok(
        "correspondence_status",
        json!({"operation_id": first["operation_id"]}),
    );
    assert_eq!(status["status"], "accepted", "{status}");
    assert!(status["delivery"]["stage"].is_string(), "{status}");
    eprintln!("Lob delivery report: {}", status["delivery"]);

    let usage = store.usage(&grant.id, Some("USD"), clock::now()).unwrap();
    assert_eq!(usage.spent_this_month_minor, 300);

    // A quote taken while permitted is refused once the grant is revoked.
    let late = assistant.call_ok(
        "correspondence_quote",
        quote_request(&connection.id, &markdown["document_id"]),
    );
    admin.revoke(&grant.id).unwrap();
    let denied = assistant.call_ok("correspondence_send", json!({"quote_id": late["quote_id"]}));
    assert_eq!(denied["accepted"], false, "{denied}");
    assert_eq!(denied["operation"], Value::Null);
    assert_eq!(denied["authorization"]["reason"], "ACTION_NOT_DELEGATED");
    assert_eq!(store.operations(None).unwrap().len(), 2);
}

fn start(data_dir: &Path, pdf_dir: &Path) -> Server {
    Server::start_with(
        data_dir,
        "assistant",
        &["--document-dir", pdf_dir.to_str().unwrap()],
    )
}
