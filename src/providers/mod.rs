//! Provider adapters (design.md §17). Adapters translate operations into
//! vendor APIs; they never grant permissions.

mod mock;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

use crate::core::{Capability, Money, Secret};
use crate::delegation::AuthorizedOperation;
use crate::documents::Document;
use crate::http::HttpError;
use crate::operations::{MailItem, Price};

pub use mock::{MockBehavior, MockProvider};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Postal,
    Fax,
    Mailbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    ApiKey,
    #[serde(rename = "oauth")]
    OAuth,
    /// The mock provider needs no credentials.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    Test,
    Live,
}

/// What a provider can do in general. A connected account may support less;
/// that is discovered per connection (design.md §3.5, §4.1).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub channels: Vec<Channel>,
    pub capabilities: Vec<Capability>,
    pub auth: AuthMethod,
    pub environments: Vec<Environment>,
}

/// Adapters are stored as `dyn ProviderAdapter`, so their async methods
/// return boxed futures.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("this provider needs a credential")]
    CredentialRequired,
    #[error("the provider rejected the credential")]
    Unauthorized,
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("{0}")]
    Other(String),
}

/// The account an adapter acts on: its environment and credential.
#[derive(Debug, Clone, Copy)]
pub struct Account<'a> {
    pub environment: Environment,
    pub credential: Option<&'a Secret>,
}

/// The provider accepted an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    /// The provider's own id for the order.
    pub reference: String,
    /// What the provider charged, when it says.
    pub charged: Option<Money>,
}

/// Why a submission did not produce a [`Submission`]. The two cases must
/// never be confused (design.md §19.2).
#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    /// The provider definitely did not accept the operation: nothing will
    /// be fulfilled or charged.
    #[error("not accepted: {0}")]
    Rejected(String),
    /// The provider may or may not have accepted it.
    #[error("outcome unknown: {0}")]
    Uncertain(String),
}

/// What the provider knows about an earlier submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Found(Submission),
    /// The provider is certain it never accepted the operation.
    NotFound,
    /// The provider cannot say (e.g. it keeps idempotency keys only briefly).
    Unknown,
}

/// The adapter contract (design.md §17.4).
pub trait ProviderAdapter: Send + Sync {
    fn info(&self) -> ProviderInfo;

    /// Check the credential against the provider and report what this
    /// account can actually do (design.md §4.1). Never guesses: a failure to
    /// reach the provider is an error, not an empty capability list.
    fn verify<'a>(
        &'a self,
        environment: Environment,
        credential: Option<&'a Secret>,
    ) -> BoxFuture<'a, Result<Vec<Capability>, ProviderError>>;

    /// The price of `item`. Never invented: a provider that cannot price
    /// returns [`crate::operations::PriceKind::Unavailable`] (§13.4).
    fn quote<'a>(&'a self, item: &'a MailItem) -> BoxFuture<'a, Result<Price, ProviderError>>;

    /// Submit `item`. Requires an [`AuthorizedOperation`], which only the
    /// policy engine can create. Its operation id is the idempotency key:
    /// submitting the same operation twice must not fulfil it twice.
    fn submit<'a>(
        &'a self,
        authorized: &'a AuthorizedOperation,
        account: Account<'a>,
        item: &'a MailItem,
        documents: &'a [Document],
    ) -> BoxFuture<'a, Result<Submission, SubmitError>>;

    /// Find an earlier submission by its operation id, to reconcile an
    /// uncertain outcome (§19.2).
    fn lookup<'a>(
        &'a self,
        operation_id: &'a str,
        account: Account<'a>,
    ) -> BoxFuture<'a, Result<Lookup, ProviderError>>;
}

/// The providers this installation knows about.
#[derive(Clone)]
pub struct Registry {
    adapters: Vec<Arc<dyn ProviderAdapter>>,
}

impl Registry {
    pub fn new(adapters: Vec<Arc<dyn ProviderAdapter>>) -> Self {
        Self { adapters }
    }

    /// The registry a default installation starts with.
    pub fn builtin() -> Self {
        Self::new(vec![Arc::new(MockProvider::default())])
    }

    pub fn list(&self) -> Vec<ProviderInfo> {
        self.adapters.iter().map(|a| a.info()).collect()
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn ProviderAdapter>> {
        self.adapters.iter().find(|a| a.info().id == id).cloned()
    }
}
