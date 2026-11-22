use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::DeliveryStage;
use super::{
    Account, AuthMethod, BoxFuture, Channel, Environment, Lookup, ProviderAdapter, ProviderError,
    ProviderInfo, Submission, SubmitError, Tracking,
};
use crate::core::{Capability, Money, Secret, clock};
use crate::delegation::AuthorizedOperation;
use crate::documents::Document;
use crate::inbound::{
    Endpoint, EndpointKind, EndpointStatus, InboundItem, InboxPage, InboxQuery, MailboxAddress,
    Scan, ScanStatus,
};
use crate::operations::{MailItem, Price, PriceKind};

/// How the mock answers a submission. Tests use this to produce every
/// outcome a real provider can.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MockBehavior {
    #[default]
    Accept,
    /// Refuse the order.
    Reject,
    /// Accept the order, but lose the response.
    UncertainAccepted,
    /// Lose the request, and report only that something went wrong.
    UncertainLost,
}

#[derive(Debug, Default)]
struct State {
    behavior: MockBehavior,
    lookup_unavailable: bool,
    accepted: HashMap<String, Submission>,
    submit_calls: u32,
}

/// A provider that performs nothing in the real world. It is used by tests
/// and is the first provider an installation can delegate against
/// (design.md §27 Phase 0). Its orders live in memory only.
#[derive(Debug, Clone, Default)]
pub struct MockProvider {
    state: Arc<Mutex<State>>,
}

impl MockProvider {
    pub fn set_behavior(&self, behavior: MockBehavior) {
        self.state.lock().unwrap().behavior = behavior;
    }

    /// Make `lookup` answer [`Lookup::Unknown`].
    pub fn set_lookup_unavailable(&self, unavailable: bool) {
        self.state.lock().unwrap().lookup_unavailable = unavailable;
    }

    pub fn submit_calls(&self) -> u32 {
        self.state.lock().unwrap().submit_calls
    }

    /// How many distinct orders the mock has accepted.
    pub fn accepted_count(&self) -> usize {
        self.state.lock().unwrap().accepted.len()
    }

    /// The mock's price list: 80 cents, plus 20 cents a page.
    pub fn price(item: &MailItem) -> Money {
        Money::new("USD", 80 + 20 * i64::from(item.page_count))
    }
}

/// The mock's one mailbox.
pub const MOCK_ENDPOINT: &str = "mock_mailbox_1";

/// The mail in the mock's mailbox, newest first: a scanned letter and an
/// unopened one. It never changes.
fn mock_inbox() -> Vec<InboundItem> {
    vec![
        InboundItem {
            item_id: "mock_item_2".into(),
            endpoint_id: Some(MOCK_ENDPOINT.into()),
            sender: Some("Example Bank".into()),
            recipient: Some("Mock Account Holder".into()),
            received_at: Some("2026-01-12T16:30:00Z".into()),
            returned_to_sender: false,
            scan_status: ScanStatus::None,
            envelope_image_url: Some("https://mock.invalid/envelopes/mock_item_2.png".into()),
            scan: None,
        },
        InboundItem {
            item_id: "mock_item_1".into(),
            endpoint_id: Some(MOCK_ENDPOINT.into()),
            sender: Some("City Water Department".into()),
            recipient: Some("Mock Account Holder".into()),
            received_at: Some("2026-01-05T15:00:00Z".into()),
            returned_to_sender: false,
            scan_status: ScanStatus::Completed,
            envelope_image_url: Some("https://mock.invalid/envelopes/mock_item_1.png".into()),
            scan: Some(Scan {
                summary: Some(
                    "A water bill for December: $42.17, due January 31. (The mock \
                     provider made this up.)"
                        .into(),
                ),
                document_url: Some("https://mock.invalid/scans/mock_item_1.pdf".into()),
            }),
        },
    ]
}

impl ProviderAdapter for MockProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "mock".into(),
            name: "Mock provider (no real-world effects)".into(),
            channels: vec![Channel::Postal, Channel::Fax, Channel::Mailbox],
            capabilities: Capability::ALL.to_vec(),
            auth: AuthMethod::None,
            environments: vec![Environment::Test],
        }
    }

    fn verify<'a>(
        &'a self,
        _: Environment,
        _: Option<&'a Secret>,
    ) -> BoxFuture<'a, Result<Vec<Capability>, ProviderError>> {
        Box::pin(async { Ok(Capability::ALL.to_vec()) })
    }

    fn quote<'a>(&'a self, item: &'a MailItem) -> BoxFuture<'a, Result<Price, ProviderError>> {
        Box::pin(async {
            Ok(Price {
                kind: PriceKind::Firm,
                amount: Some(Self::price(item)),
            })
        })
    }

    fn submit<'a>(
        &'a self,
        authorized: &'a AuthorizedOperation,
        _: Account<'a>,
        item: &'a MailItem,
        _: &'a [Document],
    ) -> BoxFuture<'a, Result<Submission, SubmitError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.submit_calls += 1;
            let key = authorized.operation_id();
            // Idempotent: a repeated operation id returns the first order.
            if let Some(existing) = state.accepted.get(key) {
                return Ok(existing.clone());
            }
            let accept = |state: &mut State| {
                let submission = Submission {
                    reference: format!("mock_ltr_{key}"),
                    charged: Some(Self::price(item)),
                };
                state.accepted.insert(key.to_owned(), submission.clone());
                submission
            };
            match state.behavior {
                MockBehavior::Accept => Ok(accept(&mut state)),
                MockBehavior::Reject => Err(SubmitError::Rejected(
                    "the mock provider was told to reject".into(),
                )),
                MockBehavior::UncertainAccepted => {
                    accept(&mut state);
                    Err(SubmitError::Uncertain("the response was lost".into()))
                }
                MockBehavior::UncertainLost => {
                    Err(SubmitError::Uncertain("the connection dropped".into()))
                }
            }
        })
    }

    fn lookup<'a>(
        &'a self,
        operation_id: &'a str,
        _: Account<'a>,
    ) -> BoxFuture<'a, Result<Lookup, ProviderError>> {
        Box::pin(async move {
            let state = self.state.lock().unwrap();
            Ok(if state.lookup_unavailable {
                Lookup::Unknown
            } else {
                match state.accepted.get(operation_id) {
                    Some(submission) => Lookup::Found(submission.clone()),
                    None => Lookup::NotFound,
                }
            })
        })
    }

    fn track<'a>(
        &'a self,
        reference: &'a str,
        _: Account<'a>,
    ) -> BoxFuture<'a, Result<Tracking, ProviderError>> {
        Box::pin(async move {
            let state = self.state.lock().unwrap();
            let known = state.accepted.values().any(|s| s.reference == reference);
            Ok(Tracking {
                stage: if known {
                    DeliveryStage::Processing
                } else {
                    DeliveryStage::Unknown
                },
                detail: Some("the mock provider mails nothing".into()),
                expected_delivery_date: None,
            })
        })
    }

    fn endpoints<'a>(
        &'a self,
        _: Account<'a>,
    ) -> BoxFuture<'a, Result<Vec<Endpoint>, ProviderError>> {
        Box::pin(async {
            Ok(vec![Endpoint {
                endpoint_id: MOCK_ENDPOINT.into(),
                kind: EndpointKind::PostalMailbox,
                status: EndpointStatus::Active,
                address: Some(MailboxAddress {
                    line1: "1 Mock Street".into(),
                    line2: Some("Suite 100".into()),
                    city: "Testville".into(),
                    state: Some("CA".into()),
                    postal_code: "94000".into(),
                    country: "US".into(),
                }),
                detail: Some("the mock provider receives nothing".into()),
            }])
        })
    }

    fn inbox<'a>(
        &'a self,
        _: Account<'a>,
        query: &'a InboxQuery,
    ) -> BoxFuture<'a, Result<InboxPage, ProviderError>> {
        Box::pin(async move {
            // The cursor is the number of matching items already returned.
            let skip = match &query.cursor {
                Some(cursor) => cursor.parse::<usize>().map_err(|_| {
                    ProviderError::InvalidItem("the cursor is not one this server returned".into())
                })?,
                None => 0,
            };
            let after = query
                .received_after
                .as_deref()
                .and_then(|t| clock::parse_rfc3339(t).ok());
            let received = |item: &InboundItem| {
                item.received_at
                    .as_deref()
                    .and_then(|t| clock::parse_rfc3339(t).ok())
            };
            let matching: Vec<InboundItem> = mock_inbox()
                .into_iter()
                .filter(|item| query.endpoint_id.is_none() || item.endpoint_id == query.endpoint_id)
                .filter(|item| after.is_none_or(|after| received(item) >= Some(after)))
                .collect();
            let limit = usize::try_from(query.limit).unwrap_or(usize::MAX);
            let end = skip.saturating_add(limit).min(matching.len());
            let items: Vec<InboundItem> = matching.iter().skip(skip).take(limit).cloned().collect();
            Ok(InboxPage {
                items,
                next_cursor: (end < matching.len()).then(|| end.to_string()),
            })
        })
    }

    fn inbound_item<'a>(
        &'a self,
        _: Account<'a>,
        item_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<InboundItem>, ProviderError>> {
        Box::pin(async move {
            Ok(mock_inbox()
                .into_iter()
                .find(|item| item.item_id == item_id))
        })
    }
}
