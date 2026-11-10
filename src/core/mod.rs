//! Types shared by every layer.

mod capability;
pub mod clock;
mod money;
mod secret;

pub use capability::{Capability, UnknownCapability};
pub use clock::Timestamp;
pub use money::Money;
pub use secret::Secret;

/// A new random identifier such as `op_3f9a1c0b7d2e`. Identifiers are
/// handles, not credentials: every lookup also checks ownership.
pub fn new_id(prefix: &str) -> String {
    let random = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}_{}", &random[..12])
}
