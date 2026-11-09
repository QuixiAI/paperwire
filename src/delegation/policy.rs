//! The policy evaluator (design.md §16). A pure function of its inputs: no
//! I/O, no clock, no model reasoning. Anything not explicitly allowed is
//! denied (§5.6), and a constraint whose input is missing fails closed.

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

use super::{Constraints, Grant};
use crate::connections::Connection;
use crate::core::{Capability, Money, Timestamp};

/// What the caller says about the proposed operation. Every field is
/// untrusted input to be checked, never a source of authority.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OperationContext {
    /// ISO 3166-1 alpha-2 country of the destination, e.g. "US".
    pub destination_country: Option<String>,
    /// Pages in the item to send.
    pub page_count: Option<u32>,
    /// Provider product, e.g. "letter".
    pub product: Option<String>,
    /// The most this operation can cost: a firm quote or an upper bound.
    pub max_cost: Option<Money>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
    ApprovalRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Reason {
    /// No grant gives this client this action on this connection.
    ActionNotDelegated,
    /// A grant covered it but has expired.
    GrantExpired,
    /// The connection is not in the `connected` state.
    ConnectionNotReady,
    /// The connected account cannot perform this action.
    CapabilityNotAvailable,
    /// A constraint applies but the context omitted (or mangled) its input.
    ContextRequired,
    DestinationNotAllowed,
    PageLimitExceeded,
    ProductNotAllowed,
    CostLimitExceeded,
    CurrencyMismatch,
    BudgetExceeded,
    DailyLimitExceeded,
    /// The grant requires the user to approve each operation.
    AdditionalApprovalRequired,
}

/// The result of evaluating one operation (design.md §16.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Evaluation {
    pub decision: Decision,
    /// Set unless the decision is `allow`.
    pub reason: Option<Reason>,
    /// Human-readable detail for `reason`.
    pub message: Option<String>,
    /// The grant that allowed the operation, or the one that came closest.
    pub grant_id: Option<String>,
    pub policy_version: Option<u32>,
}

impl Evaluation {
    pub fn is_allowed(&self) -> bool {
        self.decision == Decision::Allow
    }

    fn deny(reason: Reason, message: impl Into<String>, grant: Option<&Grant>) -> Self {
        Self {
            decision: Decision::Deny,
            reason: Some(reason),
            message: Some(message.into()),
            grant_id: grant.map(|g| g.id.clone()),
            policy_version: grant.map(|g| g.policy_version),
        }
    }
}

/// What a grant has already consumed (design.md §16.6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Spend since the start of the UTC month, in the currency of the
    /// grant's monthly budget.
    pub spent_this_month_minor: i64,
    /// Items since the start of the UTC day.
    pub items_today: u32,
}

pub struct Request<'a> {
    pub connection_id: &'a str,
    pub action: Capability,
    pub context: &'a OperationContext,
    /// The connection record, if it exists.
    pub connection: Option<&'a Connection>,
    /// Every grant held by the authenticated principal, with its usage.
    /// Grants of other principals must not be passed in.
    pub grants: &'a [(Grant, Usage)],
    pub now: Timestamp,
}

pub fn evaluate(request: &Request<'_>) -> Evaluation {
    let action = request.action;
    let covering: Vec<&(Grant, Usage)> = request
        .grants
        .iter()
        .filter(|(g, _)| {
            g.revoked_at.is_none()
                && g.actions.contains(&action)
                && g.connection_ids.iter().any(|c| c == request.connection_id)
        })
        .collect();
    let live: Vec<&(Grant, Usage)> = covering
        .iter()
        .copied()
        .filter(|(g, _)| !g.is_expired(request.now))
        .collect();

    // Whether the connection exists is not revealed to a client that holds
    // no grant on it: both cases read as "not delegated".
    let Some((first, _)) = live.first() else {
        return match covering.first() {
            Some((expired, _)) => Evaluation::deny(
                Reason::GrantExpired,
                format!("the grant for {action} on this connection has expired"),
                Some(expired),
            ),
            None => Evaluation::deny(
                Reason::ActionNotDelegated,
                format!("{action} has not been delegated to this client on this connection"),
                None,
            ),
        };
    };

    let Some(connection) = request.connection.filter(|c| c.is_ready()) else {
        return Evaluation::deny(
            Reason::ConnectionNotReady,
            "the connection is not in the connected state",
            Some(first),
        );
    };
    if !connection.capabilities.contains(&action) {
        return Evaluation::deny(
            Reason::CapabilityNotAvailable,
            format!("the connected account does not support {action}"),
            Some(first),
        );
    }

    let mut needs_approval = None;
    let mut first_failure = None;
    for (grant, usage) in live {
        match check_constraints(action, &grant.constraints, request.context, usage) {
            Ok(()) if grant.constraints.require_additional_approval => {
                needs_approval.get_or_insert(grant);
            }
            Ok(()) => {
                return Evaluation {
                    decision: Decision::Allow,
                    reason: None,
                    message: None,
                    grant_id: Some(grant.id.clone()),
                    policy_version: Some(grant.policy_version),
                };
            }
            Err(failure) => {
                first_failure.get_or_insert((grant, failure));
            }
        }
    }

    if let Some(grant) = needs_approval {
        return Evaluation {
            decision: Decision::ApprovalRequired,
            reason: Some(Reason::AdditionalApprovalRequired),
            message: Some("this grant requires the user to approve each operation".into()),
            grant_id: Some(grant.id.clone()),
            policy_version: Some(grant.policy_version),
        };
    }
    let (grant, (reason, message)) =
        first_failure.expect("a live grant either passed or failed its constraints");
    Evaluation::deny(reason, message, Some(grant))
}

type Failure = (Reason, String);

fn required<T>(value: Option<T>, field: &str, constraint: &str) -> Result<T, Failure> {
    value.ok_or_else(|| {
        (
            Reason::ContextRequired,
            format!("the grant limits {constraint}; supply context.{field}"),
        )
    })
}

fn check_constraints(
    action: Capability,
    constraints: &Constraints,
    context: &OperationContext,
    usage: &Usage,
) -> Result<(), Failure> {
    // Reads have no destination, page count or cost to constrain.
    if action.is_read() {
        return Ok(());
    }

    let has_destination = action.is_postal_send() || action == Capability::MailboxRequestForward;
    if let Some(allowed) = constraints.destination_countries.as_ref()
        && has_destination
    {
        let country = required(
            context.destination_country.as_deref(),
            "destination_country",
            "destination countries",
        )?;
        let country = country.trim();
        if !allowed.iter().any(|a| a.eq_ignore_ascii_case(country)) {
            return Err((
                Reason::DestinationNotAllowed,
                format!("destination country {country:?} is not in the grant's allowed list"),
            ));
        }
    }

    let has_pages = action.is_postal_send() || action == Capability::FaxSend;
    if let Some(max) = constraints.max_pages_per_item
        && has_pages
    {
        let pages = required(context.page_count, "page_count", "pages per item")?;
        if pages > max {
            return Err((
                Reason::PageLimitExceeded,
                format!("{pages} pages exceeds the grant's limit of {max}"),
            ));
        }
    }

    if let Some(allowed) = constraints.allowed_products.as_ref()
        && action.is_postal_send()
    {
        let product = required(context.product.as_deref(), "product", "products")?;
        if !allowed.iter().any(|a| a == product) {
            return Err((
                Reason::ProductNotAllowed,
                format!("product {product:?} is not in the grant's allowed list"),
            ));
        }
    }

    if let Some(cost) = &context.max_cost
        && cost.amount_minor < 0
    {
        return Err((
            Reason::ContextRequired,
            "context.max_cost must not be negative".into(),
        ));
    }

    if let Some(limit) = &constraints.max_cost_per_item {
        let cost = required(context.max_cost.as_ref(), "max_cost", "cost per item")?;
        match cost.le(limit) {
            Some(true) => {}
            Some(false) => {
                return Err((
                    Reason::CostLimitExceeded,
                    format!(
                        "cost {} {} exceeds the grant's per-item limit of {}",
                        cost.amount_minor, cost.currency, limit.amount_minor
                    ),
                ));
            }
            None => return Err(currency_mismatch(cost, limit)),
        }
    }

    if let Some(budget) = &constraints.monthly_budget {
        let cost = required(context.max_cost.as_ref(), "max_cost", "monthly spend")?;
        if cost.currency != budget.currency {
            return Err(currency_mismatch(cost, budget));
        }
        // Overflow counts as over budget.
        let within = usage
            .spent_this_month_minor
            .checked_add(cost.amount_minor)
            .is_some_and(|total| total <= budget.amount_minor);
        if !within {
            return Err((
                Reason::BudgetExceeded,
                format!(
                    "cost {} would exceed the monthly budget of {} {} ({} already used)",
                    cost.amount_minor,
                    budget.amount_minor,
                    budget.currency,
                    usage.spent_this_month_minor
                ),
            ));
        }
    }

    if let Some(max) = constraints.max_items_per_day
        && usage.items_today >= max
    {
        return Err((
            Reason::DailyLimitExceeded,
            format!("the grant's limit of {max} items per day is used up"),
        ));
    }

    Ok(())
}

fn currency_mismatch(cost: &Money, limit: &Money) -> Failure {
    (
        Reason::CurrencyMismatch,
        format!(
            "cost is in {} but the grant's limit is in {}",
            cost.currency, limit.currency
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::{ConnectionStatus, CredentialRef};
    use crate::core::clock::parse_rfc3339;
    use crate::providers::Environment;

    const CONN: &str = "conn_mock_1";

    fn now() -> Timestamp {
        parse_rfc3339("2026-11-09T12:00:00Z").unwrap()
    }

    fn connection() -> Connection {
        Connection {
            id: CONN.into(),
            provider: "mock".into(),
            environment: Environment::Test,
            status: ConnectionStatus::Connected,
            capabilities: Capability::ALL.to_vec(),
            credential: CredentialRef::None,
            created_at: now(),
        }
    }

    /// The example policy of design.md §5.3.
    fn grant() -> Grant {
        Grant {
            id: "grant_1".into(),
            principal_id: "assistant".into(),
            connection_ids: vec![CONN.into()],
            actions: vec![Capability::PostalSendLetter, Capability::PostalReadStatus],
            constraints: Constraints {
                destination_countries: Some(vec!["US".into()]),
                max_pages_per_item: Some(10),
                max_cost_per_item: Some(Money::new("USD", 300)),
                monthly_budget: Some(Money::new("USD", 3000)),
                max_items_per_day: Some(20),
                allowed_products: Some(vec!["letter".into()]),
                require_additional_approval: false,
            },
            policy_version: 1,
            created_at: now(),
            expires_at: Some(parse_rfc3339("2027-01-01T00:00:00Z").unwrap()),
            revoked_at: None,
        }
    }

    fn letter() -> OperationContext {
        OperationContext {
            destination_country: Some("US".into()),
            page_count: Some(2),
            product: Some("letter".into()),
            max_cost: Some(Money::new("USD", 250)),
        }
    }

    struct Case {
        grants: Vec<(Grant, Usage)>,
        connection: Option<Connection>,
        action: Capability,
        context: OperationContext,
    }

    impl Default for Case {
        fn default() -> Self {
            Self {
                grants: vec![(grant(), Usage::default())],
                connection: Some(connection()),
                action: Capability::PostalSendLetter,
                context: letter(),
            }
        }
    }

    impl Case {
        fn run(&self) -> Evaluation {
            evaluate(&Request {
                connection_id: CONN,
                action: self.action,
                context: &self.context,
                connection: self.connection.as_ref(),
                grants: &self.grants,
                now: now(),
            })
        }

        fn reason(&self) -> Option<Reason> {
            let evaluation = self.run();
            assert_ne!(evaluation.decision, Decision::Allow);
            evaluation.reason
        }
    }

    fn with_grant(edit: impl FnOnce(&mut Grant)) -> Case {
        let mut g = grant();
        edit(&mut g);
        Case {
            grants: vec![(g, Usage::default())],
            ..Case::default()
        }
    }

    fn with_context(edit: impl FnOnce(&mut OperationContext)) -> Case {
        let mut case = Case::default();
        edit(&mut case.context);
        case
    }

    #[test]
    fn allows_within_every_limit() {
        let evaluation = Case::default().run();
        assert_eq!(evaluation.decision, Decision::Allow);
        assert_eq!(evaluation.grant_id.as_deref(), Some("grant_1"));
        assert_eq!(evaluation.policy_version, Some(1));
    }

    #[test]
    fn no_grants_is_deny() {
        let case = Case {
            grants: vec![],
            ..Case::default()
        };
        assert_eq!(case.reason(), Some(Reason::ActionNotDelegated));
    }

    #[test]
    fn undelegated_action_is_denied() {
        let case = Case {
            action: Capability::FaxSend,
            ..Case::default()
        };
        assert_eq!(case.reason(), Some(Reason::ActionNotDelegated));
    }

    #[test]
    fn grant_on_another_connection_does_not_apply() {
        let case = with_grant(|g| g.connection_ids = vec!["conn_other".into()]);
        assert_eq!(case.reason(), Some(Reason::ActionNotDelegated));
    }

    #[test]
    fn revoked_grant_is_denied() {
        let case = with_grant(|g| g.revoked_at = Some(now()));
        assert_eq!(case.reason(), Some(Reason::ActionNotDelegated));
    }

    #[test]
    fn expired_grant_is_denied_at_the_instant_of_expiry() {
        let case = with_grant(|g| g.expires_at = Some(now()));
        assert_eq!(case.reason(), Some(Reason::GrantExpired));
    }

    #[test]
    fn connection_must_be_connected() {
        for status in [
            ConnectionStatus::Disconnected,
            ConnectionStatus::Expired,
            ConnectionStatus::Degraded,
            ConnectionStatus::ActionRequired,
        ] {
            let case = Case {
                connection: Some(Connection {
                    status,
                    ..connection()
                }),
                ..Case::default()
            };
            assert_eq!(
                case.reason(),
                Some(Reason::ConnectionNotReady),
                "{status:?}"
            );
        }
        let missing = Case {
            connection: None,
            ..Case::default()
        };
        assert_eq!(missing.reason(), Some(Reason::ConnectionNotReady));
    }

    #[test]
    fn account_must_support_the_action() {
        let case = Case {
            connection: Some(Connection {
                capabilities: vec![Capability::PostalReadStatus],
                ..connection()
            }),
            ..Case::default()
        };
        assert_eq!(case.reason(), Some(Reason::CapabilityNotAvailable));
    }

    #[test]
    fn each_constraint_denies_when_exceeded() {
        let cases = [
            (
                with_context(|c| c.destination_country = Some("CA".into())),
                Reason::DestinationNotAllowed,
            ),
            (
                with_context(|c| c.page_count = Some(11)),
                Reason::PageLimitExceeded,
            ),
            (
                with_context(|c| c.product = Some("postcard".into())),
                Reason::ProductNotAllowed,
            ),
            (
                with_context(|c| c.max_cost = Some(Money::new("USD", 301))),
                Reason::CostLimitExceeded,
            ),
            (
                with_context(|c| c.max_cost = Some(Money::new("EUR", 100))),
                Reason::CurrencyMismatch,
            ),
            (
                with_context(|c| c.max_cost = Some(Money::new("USD", -100))),
                Reason::ContextRequired,
            ),
        ];
        for (case, reason) in cases {
            assert_eq!(case.reason(), Some(reason));
        }
    }

    #[test]
    fn limits_are_inclusive() {
        let case = with_context(|c| {
            c.page_count = Some(10);
            c.max_cost = Some(Money::new("USD", 300));
            c.destination_country = Some(" us ".into());
        });
        assert_eq!(case.run().decision, Decision::Allow);
    }

    #[test]
    fn missing_context_fails_closed() {
        let edits: [fn(&mut OperationContext); 4] = [
            |c| c.destination_country = None,
            |c| c.page_count = None,
            |c| c.product = None,
            |c| c.max_cost = None,
        ];
        for edit in edits {
            assert_eq!(with_context(edit).reason(), Some(Reason::ContextRequired));
        }
    }

    #[test]
    fn budget_counts_prior_spend() {
        let used = |spent| Case {
            grants: vec![(
                grant(),
                Usage {
                    spent_this_month_minor: spent,
                    items_today: 0,
                },
            )],
            ..Case::default()
        };
        assert_eq!(used(2750).run().decision, Decision::Allow); // 2750 + 250 = 3000
        assert_eq!(used(2751).reason(), Some(Reason::BudgetExceeded));
        assert_eq!(used(i64::MAX).reason(), Some(Reason::BudgetExceeded));
    }

    #[test]
    fn daily_item_limit() {
        let used = |items| Case {
            grants: vec![(
                grant(),
                Usage {
                    spent_this_month_minor: 0,
                    items_today: items,
                },
            )],
            ..Case::default()
        };
        assert_eq!(used(19).run().decision, Decision::Allow);
        assert_eq!(used(20).reason(), Some(Reason::DailyLimitExceeded));
    }

    #[test]
    fn reads_skip_sending_constraints() {
        let case = Case {
            action: Capability::PostalReadStatus,
            context: OperationContext::default(),
            ..Case::default()
        };
        assert_eq!(case.run().decision, Decision::Allow);
    }

    #[test]
    fn approval_requirement_applies_after_constraints_pass() {
        let case = with_grant(|g| g.constraints.require_additional_approval = true);
        let evaluation = case.run();
        assert_eq!(evaluation.decision, Decision::ApprovalRequired);
        assert_eq!(evaluation.reason, Some(Reason::AdditionalApprovalRequired));

        // Over a limit, it is a plain denial: approval cannot widen a grant.
        let mut over = with_grant(|g| g.constraints.require_additional_approval = true);
        over.context.page_count = Some(99);
        assert_eq!(over.reason(), Some(Reason::PageLimitExceeded));
    }

    #[test]
    fn any_one_grant_that_allows_is_enough() {
        let mut narrow = grant();
        narrow.id = "grant_narrow".into();
        narrow.constraints.max_pages_per_item = Some(1);
        let case = Case {
            grants: vec![(narrow, Usage::default()), (grant(), Usage::default())],
            ..Case::default()
        };
        let evaluation = case.run();
        assert_eq!(evaluation.decision, Decision::Allow);
        assert_eq!(evaluation.grant_id.as_deref(), Some("grant_1"));
    }

    #[test]
    fn unconstrained_grant_needs_no_context() {
        let mut case = with_grant(|g| g.constraints = Constraints::default());
        case.context = OperationContext::default();
        assert_eq!(case.run().decision, Decision::Allow);
    }
}
