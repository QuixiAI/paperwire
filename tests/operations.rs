//! Sending through the mock provider: budgets, idempotency and every
//! provider outcome (design.md §16.4, §19, §28.1, §28.2).

use std::sync::Arc;

use postal_mcp::connections::{Connection, ConnectionStatus, CredentialRef, MemoryStore};
use postal_mcp::core::{Capability, Money, clock};
use postal_mcp::delegation::{Constraints, Grant, Reason, Usage};
use postal_mcp::documents::{self, DocumentFormat};
use postal_mcp::operations::{
    Address, Operation, OperationError, OperationStatus, Operations, QuoteRequest, SendOutcome,
    resolve_by_operator,
};
use postal_mcp::providers::{Environment, MockBehavior, MockProvider, Registry};
use postal_mcp::store::Store;
use time::Duration;

const CLIENT: &str = "assistant";
const CONN: &str = "conn_mock_1";
const GRANT: &str = "grant_1";

/// A one-page letter costs 100 on the mock's price list.
const LETTER: i64 = 100;

struct World {
    store: Store,
    mock: MockProvider,
    operations: Operations,
}

fn world_on(store: Store) -> World {
    let mock = MockProvider::default();
    let operations = Operations::new(
        store.clone(),
        Registry::new(vec![Arc::new(mock.clone())]),
        Arc::new(MemoryStore::default()),
    );
    World {
        store,
        mock,
        operations,
    }
}

/// A connected mock account and a grant to send letters within `constraints`.
fn world(constraints: Constraints) -> World {
    let world = world_on(Store::open_in_memory().unwrap());
    seed(&world.store, constraints);
    world
}

fn seed(store: &Store, constraints: Constraints) {
    store
        .insert_connection(&Connection {
            id: CONN.into(),
            provider: "mock".into(),
            environment: Environment::Test,
            status: ConnectionStatus::Connected,
            capabilities: Capability::ALL.to_vec(),
            credential: CredentialRef::None,
            created_at: clock::now(),
        })
        .unwrap();
    store
        .insert_grant(&Grant {
            id: GRANT.into(),
            principal_id: CLIENT.into(),
            connection_ids: vec![CONN.into()],
            actions: vec![Capability::PostalSendLetter],
            constraints,
            policy_version: 1,
            created_at: clock::now(),
            expires_at: None,
            revoked_at: None,
        })
        .unwrap();
}

fn budget(minor: i64) -> Constraints {
    Constraints {
        destination_countries: Some(vec!["US".into()]),
        max_cost_per_item: Some(Money::new("USD", 300)),
        monthly_budget: Some(Money::new("USD", minor)),
        ..Constraints::default()
    }
}

fn address(country: &str) -> Address {
    Address {
        name: "Jane Doe".into(),
        line1: "1 Main St".into(),
        line2: None,
        city: "Springfield".into(),
        state: Some("IL".into()),
        postal_code: "62701".into(),
        country: country.into(),
    }
}

impl World {
    async fn quote_for(&self, client: &str, country: &str, text: &str) -> String {
        let document =
            documents::prepare(&self.store, client, None, DocumentFormat::Text, text).unwrap();
        let request = QuoteRequest {
            connection_id: CONN.into(),
            product: "letter".into(),
            to: address(country),
            from: None,
            document_ids: vec![document.id],
        };
        let (quote, _) = self.operations.quote(client, request).await.unwrap();
        quote.id
    }

    /// A quote for a one-page domestic letter.
    async fn quote(&self) -> String {
        self.quote_for(CLIENT, "us", "Dear Jane,\nHello.").await
    }

    async fn send(&self, quote_id: &str) -> SendOutcome {
        self.operations.send(CLIENT, quote_id).await.unwrap()
    }

    async fn sent(&self, quote_id: &str) -> Operation {
        match self.send(quote_id).await {
            SendOutcome::Operation(operation) => operation,
            SendOutcome::Denied(evaluation) => panic!("denied: {evaluation:?}"),
        }
    }

    async fn denied(&self, quote_id: &str) -> Option<Reason> {
        match self.send(quote_id).await {
            SendOutcome::Denied(evaluation) => evaluation.reason,
            SendOutcome::Operation(operation) => panic!("sent: {operation:?}"),
        }
    }

    fn usage(&self) -> Usage {
        self.store.usage(GRANT, Some("USD"), clock::now()).unwrap()
    }

    fn spent(&self) -> i64 {
        self.usage().spent_this_month_minor
    }

    fn audit_kinds(&self) -> Vec<String> {
        let events = self.store.audit_events(100).unwrap();
        events.into_iter().map(|r| r.event.kind).collect()
    }
}

#[tokio::test]
async fn accepted_send_spends_the_budget_and_replays_safely() {
    let world = world(budget(3000));
    let quote = world.quote().await;

    let operation = world.sent(&quote).await;
    assert_eq!(operation.status, OperationStatus::Accepted);
    assert_eq!(operation.grant_id, GRANT);
    let reference = operation.provider_reference.clone().unwrap();
    assert!(reference.contains(&operation.id), "{reference}");
    assert_eq!(world.spent(), LETTER);
    assert_eq!(world.usage().items_today, 1);

    // Sending the same quote again is a replay: same operation, no second
    // letter, no second charge.
    let again = world.sent(&quote).await;
    assert_eq!(again, operation);
    assert_eq!(world.mock.submit_calls(), 1);
    assert_eq!(world.spent(), LETTER);

    let status = world
        .operations
        .status(CLIENT, &operation.id)
        .await
        .unwrap();
    assert_eq!(status, operation);
    assert!(
        world
            .audit_kinds()
            .contains(&"operation.accepted".to_owned())
    );
}

#[tokio::test]
async fn a_denied_send_records_reserves_and_sends_nothing() {
    let world = world(budget(3000));
    // Not a permitted destination.
    let abroad = world.quote_for(CLIENT, "CA", "Hello").await;
    assert_eq!(
        world.denied(&abroad).await,
        Some(Reason::DestinationNotAllowed)
    );
    // Too expensive: 12 pages is 80 + 12 * 20 = 320 > 300.
    let long = world
        .quote_for(CLIENT, "US", &"line\n".repeat(54 * 12))
        .await;
    assert_eq!(world.denied(&long).await, Some(Reason::CostLimitExceeded));

    assert_eq!(world.mock.submit_calls(), 0);
    assert_eq!(world.usage(), Usage::default());
    assert!(world.store.operations(None).unwrap().is_empty());
    // A denied quote can be sent once the reason is gone; here it is not.
    assert_eq!(
        world.denied(&abroad).await,
        Some(Reason::DestinationNotAllowed)
    );
}

#[tokio::test]
async fn rejected_send_fails_and_returns_the_budget() {
    let world = world(budget(3000));
    world.mock.set_behavior(MockBehavior::Reject);
    let operation = world.sent(&world.quote().await).await;
    assert_eq!(operation.status, OperationStatus::Failed);
    assert!(operation.error.unwrap().contains("reject"));
    assert_eq!(world.usage(), Usage::default());
}

#[tokio::test]
async fn uncertain_send_holds_the_budget_and_is_never_resent() {
    let world = world(budget(150));
    world.mock.set_behavior(MockBehavior::UncertainLost);
    world.mock.set_lookup_unavailable(true);
    let quote = world.quote().await;

    let operation = world.sent(&quote).await;
    assert_eq!(operation.status, OperationStatus::Uncertain);
    // The cost stays reserved, so it cannot be spent a second time.
    assert_eq!(world.spent(), LETTER);
    let another = world.quote().await;
    assert_eq!(world.denied(&another).await, Some(Reason::BudgetExceeded));

    // Sending again, or asking for status while the provider cannot say,
    // changes nothing and submits nothing.
    world.mock.set_behavior(MockBehavior::Accept);
    assert_eq!(world.sent(&quote).await.status, OperationStatus::Uncertain);
    let status = world
        .operations
        .status(CLIENT, &operation.id)
        .await
        .unwrap();
    assert_eq!(status.status, OperationStatus::Uncertain);
    assert_eq!(world.mock.submit_calls(), 1);
    assert_eq!(world.spent(), LETTER);

    // The provider can now say it never got it: failed, budget returned.
    world.mock.set_lookup_unavailable(false);
    let status = world
        .operations
        .status(CLIENT, &operation.id)
        .await
        .unwrap();
    assert_eq!(status.status, OperationStatus::Failed);
    assert_eq!(world.spent(), 0);
    assert_eq!(world.sent(&another).await.status, OperationStatus::Accepted);
}

#[tokio::test]
async fn uncertain_send_that_was_accepted_reconciles_to_accepted() {
    let world = world(budget(3000));
    world.mock.set_behavior(MockBehavior::UncertainAccepted);
    let operation = world.sent(&world.quote().await).await;
    assert_eq!(operation.status, OperationStatus::Uncertain);
    assert_eq!(operation.provider_reference, None);

    let status = world
        .operations
        .status(CLIENT, &operation.id)
        .await
        .unwrap();
    assert_eq!(status.status, OperationStatus::Accepted);
    assert!(status.provider_reference.is_some());
    assert_eq!(world.spent(), LETTER);
    assert_eq!(world.mock.accepted_count(), 1);
    assert_eq!(world.mock.submit_calls(), 1);
}

#[tokio::test]
async fn operator_settles_what_the_provider_cannot() {
    for (accepted, status, spent) in [
        (true, OperationStatus::Accepted, LETTER),
        (false, OperationStatus::Failed, 0),
    ] {
        let world = world(budget(3000));
        world.mock.set_behavior(MockBehavior::UncertainLost);
        world.mock.set_lookup_unavailable(true);
        let operation = world.sent(&world.quote().await).await;

        assert!(resolve_by_operator(&world.store, &operation.id, accepted).unwrap());
        let settled = world.store.operation(&operation.id).unwrap().unwrap();
        assert_eq!(settled.status, status);
        assert_eq!(world.spent(), spent);
        // Settled once: a second resolution changes nothing.
        assert!(!resolve_by_operator(&world.store, &operation.id, !accepted).unwrap());
        assert_eq!(world.spent(), spent);
    }
}

#[tokio::test]
async fn interrupted_send_is_reconciled_once_it_is_stale() {
    let world = world(budget(3000));
    let operation = world.sent(&world.quote().await).await;

    // Recreate the state a crash leaves behind: authorized, reserved and
    // recorded as pending, with no outcome.
    let quote = world.quote().await;
    let stale = Operation {
        id: "op_interrupted".into(),
        quote_id: quote,
        status: OperationStatus::Pending,
        provider_reference: None,
        created_at: clock::now() - Duration::minutes(10),
        updated_at: clock::now() - Duration::minutes(10),
        ..operation
    };
    world
        .store
        .atomically(|tx| {
            tx.reserve(
                &stale.id,
                GRANT,
                &Money::new("USD", LETTER),
                stale.created_at,
            )?;
            tx.insert_operation(&stale)
        })
        .unwrap();
    assert_eq!(world.spent(), 2 * LETTER);

    // The provider has no record of it, so nothing was mailed.
    let status = world.operations.status(CLIENT, &stale.id).await.unwrap();
    assert_eq!(status.status, OperationStatus::Failed);
    assert_eq!(world.spent(), LETTER);
}

#[tokio::test]
async fn authority_is_decided_at_send_time_not_quote_time() {
    let world = world(budget(3000));
    let quote = world.quote().await;
    world.store.revoke_grant(GRANT, clock::now()).unwrap();
    assert_eq!(world.denied(&quote).await, Some(Reason::ActionNotDelegated));
    assert_eq!(world.mock.submit_calls(), 0);

    let disconnected = self::world(budget(3000));
    let quote = disconnected.quote().await;
    disconnected
        .store
        .set_connection_status(CONN, ConnectionStatus::Disconnected)
        .unwrap();
    assert_eq!(
        disconnected.denied(&quote).await,
        Some(Reason::ConnectionNotReady)
    );
}

#[tokio::test]
async fn a_send_needs_a_bounded_cost_even_without_money_limits() {
    let world = world(Constraints::default());
    let operation = world.sent(&world.quote().await).await;
    assert_eq!(operation.status, OperationStatus::Accepted);
    // No budget to count against, but the item is still recorded.
    assert_eq!(world.usage().items_today, 1);
}

#[tokio::test]
async fn daily_item_limit_counts_sends() {
    let world = world(Constraints {
        max_items_per_day: Some(2),
        ..Constraints::default()
    });
    for _ in 0..2 {
        let quote = world.quote().await;
        assert_eq!(world.sent(&quote).await.status, OperationStatus::Accepted);
    }
    let third = world.quote().await;
    assert_eq!(world.denied(&third).await, Some(Reason::DailyLimitExceeded));
}

#[tokio::test]
async fn clients_cannot_use_each_others_quotes_documents_or_operations() {
    let world = world(budget(3000));
    let quote = world.quote().await;
    let operation = world.sent(&world.quote().await).await;

    let invalid = |result: Result<_, OperationError>| match result {
        Err(OperationError::Invalid(message)) => message,
        other => panic!(
            "expected a refusal, got {:?}",
            other.map(|_: SendOutcome| ())
        ),
    };
    let message = invalid(world.operations.send("other", &quote).await);
    assert!(message.contains("no quote"), "{message}");
    assert!(
        world
            .operations
            .status("other", &operation.id)
            .await
            .is_err()
    );

    // Another client has no grant, so it cannot even quote on the connection,
    // and it cannot name the first client's document.
    let document =
        documents::prepare(&world.store, CLIENT, None, DocumentFormat::Text, "hi").unwrap();
    let request = QuoteRequest {
        connection_id: CONN.into(),
        product: "letter".into(),
        to: address("US"),
        from: None,
        document_ids: vec![document.id],
    };
    let Err(OperationError::Invalid(message)) = world.operations.quote("other", request).await
    else {
        panic!("another client quoted on a connection it has no grant for");
    };
    assert!(message.contains("no accessible connection"), "{message}");
    assert_eq!(world.mock.submit_calls(), 1);
}

#[tokio::test]
async fn quote_rejects_bad_requests() {
    let world = world(budget(3000));
    let document =
        documents::prepare(&world.store, CLIENT, None, DocumentFormat::Text, "hi").unwrap();
    let request = |product: &str, country: &str, documents: Vec<String>| QuoteRequest {
        connection_id: CONN.into(),
        product: product.into(),
        to: address(country),
        from: None,
        document_ids: documents,
    };
    let bad = [
        request("postcard", "US", vec![document.id.clone()]),
        request("letter", "USA", vec![document.id.clone()]),
        request("letter", "US", vec![]),
        request("letter", "US", vec!["doc_nope".into()]),
    ];
    for request in bad {
        let result = world.operations.quote(CLIENT, request).await;
        assert!(matches!(result, Err(OperationError::Invalid(_))));
    }
}

#[tokio::test]
async fn expired_quote_cannot_be_sent() {
    let world = world(budget(3000));
    let quote_id = world.quote().await;
    let mut quote = world.store.quote(&quote_id).unwrap().unwrap();
    quote.id = "quote_old".into();
    quote.expires_at = clock::now() - Duration::minutes(1);
    world.store.insert_quote(&quote).unwrap();

    let result = world.operations.send(CLIENT, "quote_old").await;
    let Err(OperationError::Invalid(message)) = result else {
        panic!("an expired quote was accepted");
    };
    assert!(message.contains("expired"), "{message}");
    assert_eq!(world.mock.submit_calls(), 0);
}

/// Ten sends race for a budget that covers three. Two separate database
/// handles stand in for two server processes.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_sends_cannot_overspend_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let first = world_on(Store::open(dir.path()).unwrap());
    seed(&first.store, budget(3 * LETTER));
    let second = World {
        store: Store::open(dir.path()).unwrap(),
        mock: first.mock.clone(),
        operations: Operations::new(
            Store::open(dir.path()).unwrap(),
            Registry::new(vec![Arc::new(first.mock.clone())]),
            Arc::new(MemoryStore::default()),
        ),
    };

    let mut quotes = Vec::new();
    for _ in 0..10 {
        quotes.push(first.quote().await);
    }
    let mut tasks = Vec::new();
    for (i, quote) in quotes.into_iter().enumerate() {
        let operations = if i % 2 == 0 {
            first.operations.clone()
        } else {
            second.operations.clone()
        };
        tasks.push(tokio::spawn(async move {
            operations.send(CLIENT, &quote).await.unwrap()
        }));
    }

    let (mut accepted, mut over_budget) = (0, 0);
    for task in tasks {
        match task.await.unwrap() {
            SendOutcome::Operation(operation) => {
                assert_eq!(operation.status, OperationStatus::Accepted);
                accepted += 1;
            }
            SendOutcome::Denied(evaluation) => {
                assert_eq!(evaluation.reason, Some(Reason::BudgetExceeded));
                over_budget += 1;
            }
        }
    }
    assert_eq!((accepted, over_budget), (3, 7));
    assert_eq!(first.mock.accepted_count(), 3);
    assert_eq!(second.spent(), 3 * LETTER);
}
