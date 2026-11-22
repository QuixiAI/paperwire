//! The Lob adapter against a local stand-in for api.lob.com. Requests and
//! responses follow Lob's published API; none of this has run against Lob
//! itself.

mod common;

use std::sync::Arc;
use std::time::Duration;

use paperwire::connections::{
    Connection, ConnectionStatus, CredentialRef, MemoryStore, SecretStore,
};
use paperwire::core::{Capability, Money, Rate, Secret, clock};
use paperwire::delegation::{Constraints, Grant, Reason};
use paperwire::documents::{self, Document, DocumentDirs, DocumentFormat};
use paperwire::http::{ReqwestTransport, RetryPolicy, Retrying};
use paperwire::operations::{
    Address, Operation, OperationError, OperationStatus, Operations, PriceKind, ProviderPrice,
    Quote, QuoteRequest, SendOutcome,
};
use paperwire::providers::{
    DeliveryStage, Environment, LobAdapter, ProviderAdapter, ProviderError, Registry,
};
use paperwire::store::Store;
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, header_exists, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT: &str = "assistant";
const CONN: &str = "conn_lob_1";
const GRANT: &str = "grant_1";
const KEY: &str = "test_key";
/// `test_key:` in base64: the key as basic-auth username, empty password.
const BASIC: &str = "Basic dGVzdF9rZXk6";

type Lob = LobAdapter<Retrying<ReqwestTransport>>;

fn adapter(server: &MockServer) -> Lob {
    let policy = RetryPolicy {
        max_attempts: 3,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(20),
    };
    LobAdapter::with_base_url(
        Retrying::new(ReqwestTransport::new().unwrap(), policy),
        server.uri(),
    )
}

struct World {
    server: MockServer,
    store: Store,
    operations: Operations,
}

/// A connected Lob test account, a grant within `constraints` to send
/// letters and read status, and (optionally) the user's price for a letter.
async fn world(constraints: Constraints, priced: bool) -> World {
    let server = MockServer::start().await;
    let store = Store::open_in_memory().unwrap();
    let secrets = MemoryStore::default();
    secrets.set(CONN, &Secret::new(KEY)).unwrap();
    store
        .insert_connection(&Connection {
            id: CONN.into(),
            provider: "lob".into(),
            environment: Environment::Test,
            status: ConnectionStatus::Connected,
            capabilities: vec![Capability::PostalSendLetter, Capability::PostalReadStatus],
            credential: CredentialRef::Keyring,
            created_at: clock::now(),
        })
        .unwrap();
    store
        .insert_grant(&Grant {
            id: GRANT.into(),
            principal_id: CLIENT.into(),
            connection_ids: vec![CONN.into()],
            actions: vec![Capability::PostalSendLetter, Capability::PostalReadStatus],
            constraints,
            policy_version: 1,
            created_at: clock::now(),
            expires_at: None,
            revoked_at: None,
        })
        .unwrap();
    if priced {
        let price = ProviderPrice {
            provider: "lob".into(),
            product: "letter".into(),
            currency: "USD".into(),
            base: Rate::from_minor(120),
            per_extra_page: Rate::from_minor(20),
        };
        store.set_price(&price, clock::now()).unwrap();
    }
    let operations = Operations::new(
        store.clone(),
        Registry::new(vec![Arc::new(adapter(&server))]),
        Arc::new(secrets),
    );
    World {
        server,
        store,
        operations,
    }
}

fn limits() -> Constraints {
    Constraints {
        destination_countries: Some(vec!["US".into()]),
        max_cost_per_item: Some(Money::new("USD", 300)),
        monthly_budget: Some(Money::new("USD", 3000)),
        ..Constraints::default()
    }
}

fn address(name: &str) -> Address {
    Address {
        name: name.into(),
        line1: "210 King St".into(),
        line2: None,
        city: "San Francisco".into(),
        state: Some("CA".into()),
        postal_code: "94107".into(),
        country: "US".into(),
    }
}

fn letter(id: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": id,
        "object": "letter",
        "expected_delivery_date": "2026-11-16",
        "tracking_events": []
    }))
}

impl World {
    fn text(&self, format: DocumentFormat, content: &str) -> Document {
        documents::prepare(&self.store, CLIENT, None, format, content).unwrap()
    }

    fn pdf(&self, pages: u32) -> Document {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("letter.pdf");
        std::fs::write(&path, common::sample_pdf(pages)).unwrap();
        let allowed = DocumentDirs::new(&[dir.path().to_owned()]).unwrap();
        documents::import_pdf(&self.store, CLIENT, None, &path, &allowed).unwrap()
    }

    async fn quote_documents(
        &self,
        from: Option<Address>,
        documents: &[Document],
    ) -> Result<Quote, OperationError> {
        let request = QuoteRequest {
            connection_id: CONN.into(),
            product: "letter".into(),
            to: address("Jane Doe"),
            from,
            document_ids: documents.iter().map(|d| d.id.clone()).collect(),
        };
        let (quote, _) = self.operations.quote(CLIENT, request).await?;
        Ok(quote)
    }

    async fn try_quote(&self, from: Option<Address>) -> Result<Quote, OperationError> {
        let text = "Dear Jane,\n\nThank you <very> much.\n";
        let document = self.text(DocumentFormat::Text, text);
        self.quote_documents(from, &[document]).await
    }

    async fn send_documents(&self, documents: &[Document]) -> Operation {
        let quote = self
            .quote_documents(Some(address("Eric Sender")), documents)
            .await
            .unwrap();
        match self.operations.send(CLIENT, &quote.id).await.unwrap() {
            SendOutcome::Operation(operation) => operation,
            SendOutcome::Denied(evaluation) => panic!("denied: {evaluation:?}"),
        }
    }

    async fn quote(&self) -> Quote {
        self.try_quote(Some(address("Eric Sender"))).await.unwrap()
    }

    async fn send(&self) -> Operation {
        let quote = self.quote().await;
        match self.operations.send(CLIENT, &quote.id).await.unwrap() {
            SendOutcome::Operation(operation) => operation,
            SendOutcome::Denied(evaluation) => panic!("denied: {evaluation:?}"),
        }
    }

    fn spent(&self) -> i64 {
        let usage = self.store.usage(GRANT, Some("USD"), clock::now()).unwrap();
        usage.spent_this_month_minor
    }

    /// The letter-creation requests the stand-in received.
    async fn creates(&self) -> Vec<wiremock::Request> {
        let requests = self.server.received_requests().await.unwrap();
        requests
            .into_iter()
            .filter(|r| r.method.as_str() == "POST")
            .collect()
    }
}

fn header_value(request: &wiremock::Request, name: &str) -> String {
    request
        .headers
        .get(name)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn verify_checks_the_key_with_lob() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/letters"))
        .and(query_param("limit", "1"))
        .and(header("authorization", BASIC))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/letters"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({"error": {"message": "Your API key is not valid.", "status_code": 401}}),
        ))
        .with_priority(10)
        .mount(&server)
        .await;
    let lob = adapter(&server);

    let good = Secret::new(KEY);
    let capabilities = lob.verify(Environment::Test, Some(&good)).await.unwrap();
    assert_eq!(
        capabilities,
        [Capability::PostalSendLetter, Capability::PostalReadStatus]
    );

    let bad = Secret::new("test_wrong");
    let rejected = lob.verify(Environment::Test, Some(&bad)).await;
    assert!(matches!(rejected, Err(ProviderError::Unauthorized)));
    let missing = lob.verify(Environment::Test, None).await;
    assert!(matches!(missing, Err(ProviderError::CredentialRequired)));

    // A live key on a test connection is refused before any request.
    let before = server.received_requests().await.unwrap().len();
    let live = Secret::new("live_abc");
    let error = lob
        .verify(Environment::Test, Some(&live))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("not a Lob test API key"),
        "{error}"
    );
    assert!(!error.to_string().contains("live_abc"));
    assert_eq!(server.received_requests().await.unwrap().len(), before);
}

#[tokio::test]
async fn letter_is_created_with_lobs_fields_and_our_idempotency_key() {
    let world = world(limits(), true).await;
    Mock::given(method("POST"))
        .and(path("/v1/letters"))
        .and(header("authorization", BASIC))
        .and(header_exists("idempotency-key"))
        .and(header("lob-version", "2020-02-11"))
        .and(body_partial_json(json!({
            "to": {
                "name": "Jane Doe",
                "address_line1": "210 King St",
                "address_city": "San Francisco",
                "address_state": "CA",
                "address_zip": "94107",
                "address_country": "US"
            },
            "from": {"name": "Eric Sender"},
            "color": false,
            "address_placement": "insert_blank_page",
            "mail_type": "usps_first_class",
            "use_type": "operational"
        })))
        .respond_with(letter("ltr_4868c3b754655f90"))
        .expect(1)
        .mount(&world.server)
        .await;

    // One page of text plus Lob's address page: 120 + 20.
    let quote = world.quote().await;
    assert_eq!(quote.price.kind, PriceKind::Estimated);
    assert_eq!(quote.price.amount, Some(Money::new("USD", 140)));

    let outcome = world.operations.send(CLIENT, &quote.id).await.unwrap();
    let SendOutcome::Operation(operation) = outcome else {
        panic!("denied");
    };
    assert_eq!(operation.status, OperationStatus::Accepted);
    assert_eq!(
        operation.provider_reference.as_deref(),
        Some("ltr_4868c3b754655f90")
    );
    // Lob reports no charge, so the whole estimate counts as spent.
    assert_eq!(world.spent(), 140);

    let creates = world.creates().await;
    let body: Value = serde_json::from_slice(&creates[0].body).unwrap();
    assert_eq!(header_value(&creates[0], "idempotency-key"), operation.id);
    assert_eq!(body["metadata"]["operation_id"], operation.id.as_str());
    let html = body["file"].as_str().unwrap();
    assert!(html.contains("Thank you &lt;very&gt; much."), "{html}");
}

#[tokio::test]
async fn without_a_price_nothing_can_be_sent() {
    // Even a grant with no money limits needs a bounded cost.
    let world = world(Constraints::default(), false).await;
    let quote = world.quote().await;
    assert_eq!(quote.price.kind, PriceKind::Unavailable);
    assert_eq!(quote.price.amount, None);

    let outcome = world.operations.send(CLIENT, &quote.id).await.unwrap();
    let SendOutcome::Denied(evaluation) = outcome else {
        panic!("sent without a price");
    };
    assert_eq!(evaluation.reason, Some(Reason::CostUnbounded));
    assert!(world.creates().await.is_empty());
}

#[tokio::test]
async fn lob_requires_a_return_address() {
    let world = world(limits(), true).await;
    let Err(OperationError::Invalid(message)) = world.try_quote(None).await else {
        panic!("quoted without a return address");
    };
    assert!(message.contains("return address"), "{message}");
}

#[tokio::test]
async fn a_refusal_from_lob_fails_the_operation_and_returns_the_budget() {
    let world = world(limits(), true).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "error": {"message": "address_zip is required", "status_code": 422}
        })))
        .expect(1)
        .mount(&world.server)
        .await;

    let operation = world.send().await;
    assert_eq!(operation.status, OperationStatus::Failed);
    let error = operation.error.unwrap();
    assert!(error.contains("address_zip is required"), "{error}");
    assert!(!error.contains(KEY));
    assert_eq!(world.spent(), 0);
}

#[tokio::test]
async fn temporary_unavailability_is_retried_with_the_same_key() {
    let world = world(limits(), true).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&world.server)
        .await;
    Mock::given(method("POST"))
        .respond_with(letter("ltr_retry"))
        .mount(&world.server)
        .await;

    let operation = world.send().await;
    assert_eq!(operation.status, OperationStatus::Accepted);
    let creates = world.creates().await;
    assert_eq!(creates.len(), 2);
    for request in &creates {
        assert_eq!(header_value(request, "idempotency-key"), operation.id);
    }
    assert_eq!(world.spent(), 140);
}

#[tokio::test]
async fn a_server_error_is_uncertain_until_lob_lists_the_letter() {
    let world = world(limits(), true).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1) // never sent a second time
        .mount(&world.server)
        .await;
    // At first Lob lists nothing for this operation.
    Mock::given(method("GET"))
        .and(path("/v1/letters"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&world.server)
        .await;

    let operation = world.send().await;
    assert_eq!(operation.status, OperationStatus::Uncertain);
    assert_eq!(world.spent(), 140);

    // An empty list is not proof the letter does not exist.
    let status = world
        .operations
        .status(CLIENT, &operation.id)
        .await
        .unwrap();
    assert_eq!(status.status, OperationStatus::Uncertain);
    assert_eq!(world.spent(), 140);

    // Later the letter shows up under the operation id.
    Mock::given(method("GET"))
        .and(path("/v1/letters"))
        .and(query_param("metadata[operation_id]", operation.id.as_str()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "ltr_found"}]})),
        )
        .mount(&world.server)
        .await;
    let status = world
        .operations
        .status(CLIENT, &operation.id)
        .await
        .unwrap();
    assert_eq!(status.status, OperationStatus::Accepted);
    assert_eq!(status.provider_reference.as_deref(), Some("ltr_found"));
    assert_eq!(world.spent(), 140);
}

#[tokio::test]
async fn tracking_needs_read_permission_and_maps_lobs_events() {
    let world = world(limits(), true).await;
    Mock::given(method("POST"))
        .respond_with(letter("ltr_track"))
        .mount(&world.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/letters/ltr_track"))
        .and(header("authorization", BASIC))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "ltr_track",
            "expected_delivery_date": "2026-11-16",
            "tracking_events": [{"name": "Mailed"}, {"name": "In Transit"}]
        })))
        .expect(1)
        .mount(&world.server)
        .await;
    let operation = world.send().await;

    let tracking = world.operations.track(CLIENT, &operation).await.unwrap();
    let tracking = tracking.expect("the grant includes postal.read.status");
    assert_eq!(tracking.stage, DeliveryStage::InTransit);
    assert_eq!(tracking.detail.as_deref(), Some("In Transit"));
    assert_eq!(
        tracking.expected_delivery_date.as_deref(),
        Some("2026-11-16")
    );

    // Without the grant there is no report, and Lob is not asked.
    world.store.revoke_grant(GRANT, clock::now()).unwrap();
    let tracking = world.operations.track(CLIENT, &operation).await.unwrap();
    assert_eq!(tracking, None);
}

#[tokio::test]
async fn a_pdf_is_uploaded_unchanged_as_multipart() {
    let world = world(limits(), true).await;
    Mock::given(method("POST"))
        .and(path("/v1/letters"))
        .and(header("authorization", BASIC))
        .respond_with(letter("ltr_pdf"))
        .expect(1)
        .mount(&world.server)
        .await;

    let pdf = world.pdf(3);
    assert_eq!((pdf.page_count, pdf.page_count_estimated), (3, false));
    // Three pages plus Lob's address page: 120 + 3 * 20.
    let quote = world
        .quote_documents(Some(address("Eric Sender")), std::slice::from_ref(&pdf))
        .await
        .unwrap();
    assert_eq!(quote.price.amount, Some(Money::new("USD", 180)));
    let SendOutcome::Operation(operation) = world.operations.send(CLIENT, &quote.id).await.unwrap()
    else {
        panic!("denied");
    };
    assert_eq!(operation.status, OperationStatus::Accepted);

    let creates = world.creates().await;
    let request = &creates[0];
    assert!(header_value(request, "content-type").starts_with("multipart/form-data; boundary="));
    assert_eq!(header_value(request, "idempotency-key"), operation.id);
    // The file's bytes are in the body exactly as imported.
    let found = request
        .body
        .windows(pdf.content.len())
        .any(|window| window == pdf.content.as_slice());
    assert!(found, "the PDF bytes are not in the upload");
    let body = String::from_utf8_lossy(&request.body);
    for expected in [
        "name=\"to[name]\"",
        "Jane Doe",
        "name=\"to[address_zip]\"",
        "name=\"from[address_line1]\"",
        "name=\"address_placement\"",
        "insert_blank_page",
        "name=\"use_type\"",
        "name=\"metadata[operation_id]\"",
        "filename=\"letter.pdf\"",
        "Content-Type: application/pdf",
    ] {
        assert!(
            body.contains(expected),
            "{expected} missing from the upload"
        );
    }
    assert!(body.contains(&operation.id));
    // An address line that was not given is not sent as an empty field.
    assert!(!body.contains("address_line2"));
}

#[tokio::test]
async fn a_pdf_cannot_be_mixed_with_other_documents() {
    let world = world(limits(), true).await;
    let documents = [world.pdf(1), world.text(DocumentFormat::Text, "cover note")];
    let result = world
        .quote_documents(Some(address("Eric Sender")), &documents)
        .await;
    let Err(OperationError::Invalid(message)) = result else {
        panic!("quoted a mixed letter");
    };
    assert!(message.contains("single PDF"), "{message}");
}

#[tokio::test]
async fn markdown_is_sent_as_formatted_html_and_text_documents_combine() {
    let world = world(limits(), true).await;
    Mock::given(method("POST"))
        .respond_with(letter("ltr_md"))
        .mount(&world.server)
        .await;
    let documents = [
        world.text(DocumentFormat::Markdown, "# Notice\n\nPlease **reply**.\n"),
        world.text(DocumentFormat::Text, "Enclosure <1>"),
    ];
    let operation = world.send_documents(&documents).await;
    assert_eq!(operation.status, OperationStatus::Accepted);

    let creates = world.creates().await;
    let body: Value = serde_json::from_slice(&creates[0].body).unwrap();
    let html = body["file"].as_str().unwrap();
    assert!(html.contains("<h1>Notice</h1>"), "{html}");
    assert!(html.contains("<strong>reply</strong>"), "{html}");
    assert!(html.contains("Enclosure &lt;1&gt;"), "{html}");
    assert!(html.contains("doc text next"), "{html}");
}

#[tokio::test]
async fn text_too_long_for_lobs_inline_limit_fails_before_anything_is_sent() {
    let world = world(Constraints::default(), true).await;
    let long = world.text(DocumentFormat::Text, &"word ".repeat(2500));
    let operation = world.send_documents(&[long]).await;
    assert_eq!(operation.status, OperationStatus::Failed);
    assert!(operation.error.unwrap().contains("as a PDF"));
    assert!(world.creates().await.is_empty());
    assert_eq!(world.spent(), 0);
}
