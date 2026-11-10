//! Delegated authority (design.md §5, §16): what the user has allowed an AI
//! client to do with a connected account.
//!
//! Grants are created and revoked only by the control plane (`admin`). The
//! MCP side can read and evaluate them, never change them.

pub mod policy;

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

use crate::connections::Connection;
use crate::core::{Capability, Money, Timestamp};
use crate::store::{AuditEvent, Store, StoreError, Tx};

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

/// Proof that the policy engine allowed one specific operation and held
/// its cost against the grant's budget.
///
/// Adapters require one to submit anything. Its fields are private and the
/// only constructor is [`authorize`], so no code path reaches a provider
/// without a policy decision, and nothing an AI client sends can stand in
/// for one (design.md §17.5, §20.6).
#[derive(Debug)]
pub struct AuthorizedOperation {
    operation_id: String,
    principal_id: String,
    connection_id: String,
    action: Capability,
    grant_id: String,
    policy_version: u32,
    max_cost: Money,
}

impl AuthorizedOperation {
    /// Also the idempotency key sent to the provider (§19.1).
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }

    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    pub fn action(&self) -> Capability {
        self.action
    }

    pub fn grant_id(&self) -> &str {
        &self.grant_id
    }

    pub fn policy_version(&self) -> u32 {
        self.policy_version
    }

    /// The amount reserved: the most this operation may cost.
    pub fn max_cost(&self) -> &Money {
        &self.max_cost
    }
}

/// The connections `principal` holds an active grant on. A client cannot
/// learn of any other connection.
pub fn visible_connections(
    store: &Store,
    principal: &str,
    now: Timestamp,
) -> Result<Vec<Connection>, StoreError> {
    let grants = store.grants(Some(principal))?;
    let visible: Vec<&String> = grants
        .iter()
        .filter(|g| g.is_active(now))
        .flat_map(|g| &g.connection_ids)
        .collect();
    let mut connections = store.connections()?;
    connections.retain(|c| visible.contains(&&c.id));
    Ok(connections)
}

fn evaluate_in(
    tx: &Tx<'_>,
    kind: &str,
    principal: &str,
    connection_id: &str,
    action: Capability,
    context: &OperationContext,
    now: Timestamp,
) -> Result<Evaluation, StoreError> {
    let grants = tx
        .grants(Some(principal))?
        .into_iter()
        .map(|grant| {
            let currency = grant
                .constraints
                .monthly_budget
                .as_ref()
                .map(|budget| budget.currency.as_str());
            let usage = tx.usage(&grant.id, currency, now)?;
            Ok((grant, usage))
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let connection = tx.connection(connection_id)?;

    let evaluation = policy::evaluate(&policy::Request {
        connection_id,
        action,
        context,
        connection: connection.as_ref(),
        grants: &grants,
        now,
    });

    tx.audit(
        &AuditEvent {
            kind: kind.into(),
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

/// Evaluate a proposed operation for `principal` against current state, and
/// record the decision (design.md §6.5). A dry run: nothing is reserved.
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
    store.atomically(|tx| {
        evaluate_in(
            tx,
            "authorization.checked",
            principal,
            connection_id,
            action,
            context,
            now,
        )
    })
}

/// Decide whether `operation_id` may run and, if so, hold its cost against
/// the grant. This is the enforcement point immediately before execution
/// (design.md §16.3), and the only source of an [`AuthorizedOperation`].
///
/// `tx` must come from [`Store::atomically`]: evaluating and reserving in
/// one write transaction is what stops concurrent requests from each
/// seeing the same unspent budget (§16.4).
///
/// A send must have a bounded cost (§13.4), so `context.max_cost` is
/// required even when the grant sets no money limit.
pub fn authorize(
    tx: &Tx<'_>,
    operation_id: &str,
    principal: &str,
    connection_id: &str,
    action: Capability,
    context: &OperationContext,
    now: Timestamp,
) -> Result<Result<AuthorizedOperation, Evaluation>, StoreError> {
    let evaluation = evaluate_in(
        tx,
        "authorization.enforced",
        principal,
        connection_id,
        action,
        context,
        now,
    )?;
    let (Decision::Allow, Some(grant_id), Some(policy_version)) = (
        evaluation.decision,
        evaluation.grant_id.clone(),
        evaluation.policy_version,
    ) else {
        return Ok(Err(evaluation));
    };
    let Some(max_cost) = context.max_cost.clone() else {
        return Ok(Err(Evaluation {
            decision: Decision::Deny,
            reason: Some(Reason::CostUnbounded),
            message: Some(
                "the provider gave no price, so the cost of this operation is not bounded".into(),
            ),
            grant_id: Some(grant_id),
            policy_version: Some(policy_version),
        }));
    };
    tx.reserve(operation_id, &grant_id, &max_cost, now)?;
    Ok(Ok(AuthorizedOperation {
        operation_id: operation_id.to_owned(),
        principal_id: principal.to_owned(),
        connection_id: connection_id.to_owned(),
        action,
        grant_id,
        policy_version,
        max_cost,
    }))
}
