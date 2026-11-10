use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::DeliveryStage;
use super::{
    Account, AuthMethod, BoxFuture, Channel, Environment, Lookup, ProviderAdapter, ProviderError,
    ProviderInfo, Submission, SubmitError, Tracking,
};
use crate::core::{Capability, Money, Secret};
use crate::delegation::AuthorizedOperation;
use crate::documents::Document;
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
}
