use super::{AuthMethod, Channel, Environment, ProviderAdapter, ProviderInfo};
use crate::core::Capability;

/// A provider that performs nothing in the real world. It is used by tests
/// and is the first provider an installation can delegate against
/// (design.md §27 Phase 0).
#[derive(Debug, Clone, Copy, Default)]
pub struct MockProvider;

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
}
