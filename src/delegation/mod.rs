//! Delegated authority (design.md §5, §16): what the user has allowed an AI
//! client to do with a connected account.
//!
//! Grants are created and revoked only by the control plane (`admin`). The
//! MCP side can read and evaluate them, never change them.

pub mod policy;

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

use crate::core::{Capability, Money, Timestamp};
use crate::store::{AuditEvent, Store, StoreError};

pub use policy::{Decision, Evaluation, OperationContext, Reason, Usage};

/// Limits on a grant (design.md §5.3). An absent limit does not restrict.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Constraints {
    /// ISO 3166-1 alpha-2 codes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_countries: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_pages_per_item: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cost_per_item: Option<Money>,
    /// Per UTC calendar month.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly_budget: Option<Money>,
    /// Per UTC day.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_items_per_day: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_products: Option<Vec<String>>,
    /// The user must approve each operation even when it is within limits.
    pub require_additional_approval: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub id: String,
    /// The client this grant is for. Comes from server configuration, never
    /// from tool arguments (design.md §5.2).
    pub principal_id: String,
    pub connection_ids: Vec<String>,
    pub actions: Vec<Capability>,
    pub constraints: Constraints,
    pub policy_version: u32,
    pub created_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub revoked_at: Option<Timestamp>,
}

impl Grant {
    pub fn is_expired(&self, now: Timestamp) -> bool {
        self.expires_at.is_some_and(|at| at <= now)
    }

    /// Neither revoked nor expired.
    pub fn is_active(&self, now: Timestamp) -> bool {
        self.revoked_at.is_none() && !self.is_expired(now)
    }
}

/// Evaluate a proposed operation for `principal` against current state, and
/// record the decision (design.md §6.5).
///
/// State is read on every call, so a revocation takes effect on the next
/// evaluation (§6.3).
pub fn check(
    store: &Store,
    principal: &str,
    connection_id: &str,
    action: Capability,
    context: &OperationContext,
    now: Timestamp,
) -> Result<Evaluation, StoreError> {
    let grants = store
        .grants(Some(principal))?
        .into_iter()
        .map(|grant| {
            let currency = grant
                .constraints
                .monthly_budget
                .as_ref()
                .map(|budget| budget.currency.as_str());
            let usage = store.usage(&grant.id, currency, now)?;
            Ok((grant, usage))
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let connection = store.connection(connection_id)?;

    let evaluation = policy::evaluate(&policy::Request {
        connection_id,
        action,
        context,
        connection: connection.as_ref(),
        grants: &grants,
        now,
    });

    store.audit(
        &AuditEvent {
            kind: "authorization.checked".into(),
            principal_id: Some(principal.to_owned()),
            connection_id: Some(connection_id.to_owned()),
            grant_id: evaluation.grant_id.clone(),
            action: Some(action.to_string()),
            decision: Some(crate::store::enum_text(&evaluation.decision)),
            reason: evaluation.reason.as_ref().map(crate::store::enum_text),
            policy_version: evaluation.policy_version,
            detail: None,
        },
        now,
    )?;
    Ok(evaluation)
}
