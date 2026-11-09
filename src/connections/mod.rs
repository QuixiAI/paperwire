//! Connected provider accounts (design.md §4).
//!
//! A connection says which account exists and what it can do. It grants the
//! AI nothing: that takes a delegation grant (§5.6).

mod secrets;

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

use crate::core::{Capability, Timestamp};
use crate::providers::Environment;

pub use secrets::{KeyringStore, MemoryStore, SecretError, SecretStore, resolve};

/// Connection states (design.md §4.6). Only `Connected` permits operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    NotConnected,
    Connecting,
    ActionRequired,
    Connected,
    Degraded,
    Expired,
    Disconnected,
}

/// Where a connection's credential lives. The credential itself is never
/// stored in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRef {
    /// The provider needs no credential (the mock provider).
    None,
    /// In the OS credential store, under the connection id.
    Keyring,
    /// In the named environment variable, read at use time. For machines
    /// without a credential store.
    Env(String),
}

impl CredentialRef {
    pub(crate) fn encode(&self) -> String {
        match self {
            Self::None => "none".into(),
            Self::Keyring => "keyring".into(),
            Self::Env(name) => format!("env:{name}"),
        }
    }

    pub(crate) fn decode(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "keyring" => Some(Self::Keyring),
            _ => s.strip_prefix("env:").map(|n| Self::Env(n.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// Stable internal id, e.g. `conn_lob_1a2b3c`.
    pub id: String,
    pub provider: String,
    pub environment: Environment,
    pub status: ConnectionStatus,
    /// What this account can do, as verified with the provider.
    pub capabilities: Vec<Capability>,
    pub credential: CredentialRef,
    pub created_at: Timestamp,
}

impl Connection {
    pub fn is_ready(&self) -> bool {
        self.status == ConnectionStatus::Connected
    }
}
