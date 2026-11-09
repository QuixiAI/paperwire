//! Provider adapters (design.md §17). Adapters translate operations into
//! vendor APIs; they never grant permissions.

mod mock;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

use crate::core::{Capability, Secret};
use crate::http::HttpError;

pub use mock::MockProvider;

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

/// The adapter contract. It grows by milestone (execute with an
/// `AuthorizedOperation` comes with operations); only what is implemented
/// today is declared.
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
        Self::new(vec![Arc::new(MockProvider)])
    }

    pub fn list(&self) -> Vec<ProviderInfo> {
        self.adapters.iter().map(|a| a.info()).collect()
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn ProviderAdapter>> {
        self.adapters.iter().find(|a| a.info().id == id).cloned()
    }
}
