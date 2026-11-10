//! The trusted control plane (design.md §6, §20.3): connect accounts, grant
//! and revoke permissions, read the audit log.
//!
//! Nothing here is reachable from MCP tools. Connecting and granting widen
//! what the AI can do, so both require the user to confirm on an interactive
//! terminal; revoking and disconnecting only narrow it and do not.

pub mod cli;

use std::io::{BufRead, IsTerminal, Write};

use crate::connections::{
    Connection, ConnectionStatus, CredentialRef, SecretError, SecretStore, resolve,
};
use crate::core::{Capability, Secret, Timestamp, clock, new_id};
use crate::delegation::{Constraints, Grant};
use crate::operations::{self, Operation, OperationStatus, ProviderPrice};
use crate::providers::{AuthMethod, Environment, ProviderError, Registry};
use crate::store::{AuditEvent, AuditRecord, Store, StoreError, enum_text};

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error(
        "this command changes what the AI may do, so it must be confirmed on an \
         interactive terminal; run it yourself in a terminal"
    )]
    NotInteractive,
    #[error("not confirmed; nothing was changed")]
    Declined,
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Secret(#[from] SecretError),
    #[error("could not verify the connection: {0}")]
    Provider(#[from] ProviderError),
    #[error("terminal: {0}")]
    Io(#[from] std::io::Error),
}

/// The user's side of the control plane.
pub trait Terminal {
    /// Show `summary` and ask for confirmation. Fails with
    /// [`AdminError::NotInteractive`] when no person can be asked.
    fn confirm(&mut self, summary: &str) -> Result<bool, AdminError>;

    /// Read a credential without echoing it.
    fn read_secret(&mut self, prompt: &str) -> Result<Secret, AdminError>;
}

/// The real terminal. It refuses unless stdin and stderr are both
/// terminals. There is deliberately no way to skip this (design.md §6.2).
pub struct InteractiveTerminal;

impl InteractiveTerminal {
    fn require_tty() -> Result<(), AdminError> {
        if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
            Ok(())
        } else {
            Err(AdminError::NotInteractive)
        }
    }
}

impl Terminal for InteractiveTerminal {
    fn confirm(&mut self, summary: &str) -> Result<bool, AdminError> {
        Self::require_tty()?;
        let mut err = std::io::stderr().lock();
        write!(err, "{summary}\n\nType \"yes\" to confirm: ")?;
        err.flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        Ok(answer.trim() == "yes")
    }

    fn read_secret(&mut self, prompt: &str) -> Result<Secret, AdminError> {
        Self::require_tty()?;
        Ok(Secret::new(rpassword::prompt_password(prompt)?))
    }
}

/// A grant as requested by the user, before it has an id.
#[derive(Debug, Clone)]
pub struct GrantRequest {
    pub principal_id: String,
    pub connection_ids: Vec<String>,
    pub actions: Vec<Capability>,
    pub constraints: Constraints,
    pub expires_at: Option<Timestamp>,
}

pub struct Admin<'a> {
    pub store: &'a Store,
    pub providers: &'a Registry,
    pub secrets: &'a dyn SecretStore,
    pub terminal: &'a mut dyn Terminal,
}

impl Admin<'_> {
    fn confirm(&mut self, summary: &str) -> Result<(), AdminError> {
        if self.terminal.confirm(summary)? {
            Ok(())
        } else {
            Err(AdminError::Declined)
        }
    }

    /// Connect a provider account (design.md §4.1): confirm, obtain the
    /// credential, verify it with the provider, then store the connection.
    /// Connecting grants the AI nothing (§5.6).
    pub async fn connect(
        &mut self,
        provider: &str,
        environment: Environment,
        from_env: Option<String>,
    ) -> Result<Connection, AdminError> {
        let adapter = self
            .providers
            .get(provider)
            .ok_or_else(|| AdminError::Invalid(format!("unknown provider {provider:?}")))?;
        let info = adapter.info();
        if !info.environments.contains(&environment) {
            return Err(AdminError::Invalid(format!(
                "{} has no {} environment",
                info.id,
                enum_text(&environment)
            )));
        }
        let credential = match (info.auth, from_env) {
            (AuthMethod::None, None) => CredentialRef::None,
            (AuthMethod::None, Some(_)) => {
                return Err(AdminError::Invalid(format!(
                    "{} takes no credential",
                    info.id
                )));
            }
            (AuthMethod::ApiKey, Some(variable)) => CredentialRef::Env(variable),
            (AuthMethod::ApiKey, None) => CredentialRef::Keyring,
            (AuthMethod::OAuth, _) => {
                return Err(AdminError::Invalid(
                    "OAuth connections are not supported yet".into(),
                ));
            }
        };

        self.confirm(&format!(
            "Connect a {} account ({} environment).\n\
             This stores the connection only. The AI can do nothing with it until you \
             grant permissions with `postal-mcp admin grant`.",
            info.name,
            enum_text(&environment),
        ))?;

        let id = new_id(&format!("conn_{}", info.id));
        let secret = match &credential {
            CredentialRef::Keyring => {
                let secret = self
                    .terminal
                    .read_secret(&format!("{} API key: ", info.name))?;
                if secret.expose().is_empty() {
                    return Err(AdminError::Invalid("no key entered".into()));
                }
                Some(secret)
            }
            other => resolve(other, &id, self.secrets)?,
        };
        let capabilities = adapter.verify(environment, secret.as_ref()).await?;

        let connection = Connection {
            id,
            provider: info.id,
            environment,
            status: ConnectionStatus::Connected,
            capabilities,
            credential,
            created_at: clock::now(),
        };
        if let (CredentialRef::Keyring, Some(secret)) = (&connection.credential, &secret) {
            self.secrets.set(&connection.id, secret)?;
        }
        if let Err(e) = self.store.insert_connection(&connection) {
            // Do not leave an orphaned credential behind.
            let _ = self.secrets.delete(&connection.id);
            return Err(e.into());
        }
        self.store.audit(
            &AuditEvent {
                kind: "connection.created".into(),
                connection_id: Some(connection.id.clone()),
                detail: Some(format!(
                    "{} {}",
                    connection.provider,
                    enum_text(&environment)
                )),
                ..AuditEvent::default()
            },
            connection.created_at,
        )?;
        Ok(connection)
    }

    /// Disconnect an account (design.md §4.7). New operations stop at once,
    /// because the policy evaluator requires a connected account; the stored
    /// credential is deleted; history is kept.
    pub fn disconnect(&mut self, connection_id: &str) -> Result<(), AdminError> {
        let connection = self
            .store
            .connection(connection_id)?
            .ok_or_else(|| AdminError::Invalid(format!("no connection {connection_id:?}")))?;
        self.store
            .set_connection_status(connection_id, ConnectionStatus::Disconnected)?;
        if connection.credential == CredentialRef::Keyring {
            self.secrets.delete(connection_id)?;
        }
        self.store.audit(
            &AuditEvent {
                kind: "connection.disconnected".into(),
                connection_id: Some(connection_id.to_owned()),
                ..AuditEvent::default()
            },
            clock::now(),
        )?;
        Ok(())
    }

    /// Create a delegation grant (design.md §6.2) after the user confirms
    /// exactly what it allows.
    pub fn grant(&mut self, request: GrantRequest) -> Result<Grant, AdminError> {
        let now = clock::now();
        let invalid = |message: String| Err(AdminError::Invalid(message));
        if request.principal_id.trim().is_empty() {
            return invalid("a client name is required".into());
        }
        if request.actions.is_empty() {
            return invalid("at least one action is required".into());
        }
        if request.connection_ids.is_empty() {
            return invalid("at least one connection is required".into());
        }
        if request.expires_at.is_some_and(|at| at <= now) {
            return invalid("the expiry is in the past".into());
        }
        for limit in [
            &request.constraints.max_cost_per_item,
            &request.constraints.monthly_budget,
        ]
        .into_iter()
        .flatten()
        {
            if limit.amount_minor < 0 {
                return invalid("money limits must not be negative".into());
            }
        }
        for id in &request.connection_ids {
            let Some(connection) = self.store.connection(id)? else {
                return invalid(format!("no connection {id:?}"));
            };
            if !connection.is_ready() {
                return invalid(format!("connection {id} is not connected"));
            }
            // A grant cannot exceed what the account can do (§5.5).
            if let Some(missing) = request
                .actions
                .iter()
                .find(|a| !connection.capabilities.contains(a))
            {
                return invalid(format!("connection {id} does not support {missing}"));
            }
        }

        let grant = Grant {
            id: new_id("grant"),
            principal_id: request.principal_id.trim().to_owned(),
            connection_ids: request.connection_ids,
            actions: request.actions,
            constraints: request.constraints,
            policy_version: 1,
            created_at: now,
            expires_at: request.expires_at,
            revoked_at: None,
        };
        self.confirm(&format!(
            "Grant these permissions to an AI client:\n\n{}",
            describe_grant(&grant)
        ))?;

        self.store.insert_grant(&grant)?;
        self.store.audit(
            &AuditEvent {
                kind: "grant.created".into(),
                principal_id: Some(grant.principal_id.clone()),
                grant_id: Some(grant.id.clone()),
                policy_version: Some(grant.policy_version),
                detail: Some(describe_grant(&grant)),
                ..AuditEvent::default()
            },
            now,
        )?;
        Ok(grant)
    }

    /// Revoke a grant (design.md §6.3). Takes effect on the next evaluation.
    pub fn revoke(&mut self, grant_id: &str) -> Result<(), AdminError> {
        let now = clock::now();
        let Some(grant) = self.store.grant(grant_id)? else {
            return Err(AdminError::Invalid(format!("no grant {grant_id:?}")));
        };
        if !self.store.revoke_grant(grant_id, now)? {
            return Err(AdminError::Invalid(format!(
                "grant {grant_id} is already revoked"
            )));
        }
        self.store.audit(
            &AuditEvent {
                kind: "grant.revoked".into(),
                principal_id: Some(grant.principal_id),
                grant_id: Some(grant_id.to_owned()),
                policy_version: Some(grant.policy_version),
                ..AuditEvent::default()
            },
            now,
        )?;
        Ok(())
    }

    /// Settle an operation whose outcome the provider cannot report
    /// (design.md §19.2 step 5), after checking with the provider yourself.
    /// Marking it failed returns its held cost to the budget, so this is
    /// confirmed like a grant.
    pub fn resolve(&mut self, operation_id: &str, accepted: bool) -> Result<(), AdminError> {
        let Some(operation) = self.store.operation(operation_id)? else {
            return Err(AdminError::Invalid(format!(
                "no operation {operation_id:?}"
            )));
        };
        if !matches!(
            operation.status,
            OperationStatus::Uncertain | OperationStatus::Pending
        ) {
            return Err(AdminError::Invalid(format!(
                "operation {operation_id} is already settled ({})",
                enum_text(&operation.status)
            )));
        }
        self.confirm(&if accepted {
            format!(
                "Mark operation {operation_id} as ACCEPTED by the provider.\n\
                 Its reserved cost will count as spent."
            )
        } else {
            format!(
                "Mark operation {operation_id} as FAILED (never accepted by the provider).\n\
                 Its reserved cost returns to the client's budget. If the provider did \
                 accept it, the client could then overspend."
            )
        })?;
        if !operations::resolve_by_operator(self.store, operation_id, accepted)? {
            return Err(AdminError::Invalid(format!(
                "operation {operation_id} was settled in the meantime"
            )));
        }
        Ok(())
    }

    /// Set your price for a provider's product, for providers that cannot
    /// quote. Spending limits are checked against it, so a price set too
    /// low lets the AI spend more than its limits intend: confirmed like a
    /// grant.
    pub fn set_price(&mut self, price: ProviderPrice) -> Result<(), AdminError> {
        if self.providers.get(&price.provider).is_none() {
            return Err(AdminError::Invalid(format!(
                "unknown provider {:?}",
                price.provider
            )));
        }
        if price.base.amount_minor < 0 || price.per_extra_page_minor < 0 {
            return Err(AdminError::Invalid("prices must not be negative".into()));
        }
        self.confirm(&format!(
            "Set the price of a {} {} to {} {} for the first page plus {} for each \
             further page (in minor units, e.g. cents).\n\
             Spending limits are checked against this price. If the provider charges \
             more, the AI can spend more than its limits allow.",
            price.provider,
            price.product,
            price.base.amount_minor,
            price.base.currency,
            price.per_extra_page_minor,
        ))?;
        let now = clock::now();
        self.store.set_price(&price, now)?;
        self.store.audit(
            &AuditEvent {
                kind: "price.set".into(),
                detail: Some(format!(
                    "{} {}: {} {} + {} per extra page",
                    price.provider,
                    price.product,
                    price.base.amount_minor,
                    price.base.currency,
                    price.per_extra_page_minor
                )),
                ..AuditEvent::default()
            },
            now,
        )?;
        Ok(())
    }

    pub fn prices(&self) -> Result<Vec<ProviderPrice>, AdminError> {
        Ok(self.store.prices()?)
    }

    pub fn operations(&self, principal: Option<&str>) -> Result<Vec<Operation>, AdminError> {
        Ok(self.store.operations(principal)?)
    }

    pub fn connections(&self) -> Result<Vec<Connection>, AdminError> {
        Ok(self.store.connections()?)
    }

    pub fn grants(&self, principal: Option<&str>) -> Result<Vec<Grant>, AdminError> {
        Ok(self.store.grants(principal)?)
    }

    pub fn audit(&self, limit: u32) -> Result<Vec<AuditRecord>, AdminError> {
        Ok(self.store.audit_events(limit)?)
    }
}

fn join<T: ToString>(items: &[T]) -> String {
    items
        .iter()
        .map(T::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every term of a grant, one per line: what the user confirms and what the
/// audit log keeps.
pub fn describe_grant(grant: &Grant) -> String {
    let c = &grant.constraints;
    let mut lines = vec![
        format!("  client:       {}", grant.principal_id),
        format!("  connections:  {}", join(&grant.connection_ids)),
        format!("  actions:      {}", join(&grant.actions)),
    ];
    let mut limit = |label: &str, value: Option<String>| {
        lines.push(format!(
            "  {label:<13} {}",
            value.unwrap_or_else(|| "no limit".into())
        ));
    };
    let money = |m: &crate::core::Money| {
        format!(
            "{} {} (in minor units, e.g. cents)",
            m.amount_minor, m.currency
        )
    };
    limit("countries:", c.destination_countries.as_deref().map(join));
    limit("max pages:", c.max_pages_per_item.map(|n| n.to_string()));
    limit("max cost:", c.max_cost_per_item.as_ref().map(money));
    limit("month budget:", c.monthly_budget.as_ref().map(money));
    limit("items/day:", c.max_items_per_day.map(|n| n.to_string()));
    limit("products:", c.allowed_products.as_deref().map(join));
    lines.push(format!(
        "  approval:     {}",
        if c.require_additional_approval {
            "you approve each operation"
        } else {
            "none; the AI acts unattended within these limits"
        }
    ));
    lines.push(format!(
        "  expires:      {}",
        grant
            .expires_at
            .map_or_else(|| "never (until revoked)".into(), clock::to_rfc3339)
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::connections::MemoryStore;
    use crate::core::Money;
    use crate::delegation::AuthorizedOperation;
    use crate::documents::Document;
    use crate::operations::{MailItem, Price};
    use crate::providers::{
        Account, BoxFuture, Channel, Lookup, MockProvider, ProviderAdapter, ProviderInfo,
        Submission, SubmitError, Tracking,
    };

    /// Answers prompts from a script and records what it was shown.
    #[derive(Default)]
    struct Scripted {
        interactive: bool,
        answer: bool,
        secret: Option<&'static str>,
        shown: Vec<String>,
    }

    impl Scripted {
        fn yes() -> Self {
            Self {
                interactive: true,
                answer: true,
                ..Self::default()
            }
        }
    }

    impl Terminal for Scripted {
        fn confirm(&mut self, summary: &str) -> Result<bool, AdminError> {
            if !self.interactive {
                return Err(AdminError::NotInteractive);
            }
            self.shown.push(summary.to_owned());
            Ok(self.answer)
        }

        fn read_secret(&mut self, _: &str) -> Result<Secret, AdminError> {
            Ok(Secret::new(self.secret.expect("unexpected secret prompt")))
        }
    }

    /// An API-key provider that accepts exactly one key.
    struct KeyedProvider;

    impl ProviderAdapter for KeyedProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: "keyed".into(),
                name: "Keyed".into(),
                channels: vec![Channel::Postal],
                capabilities: vec![Capability::PostalSendLetter],
                auth: AuthMethod::ApiKey,
                environments: vec![Environment::Test],
            }
        }

        fn verify<'a>(
            &'a self,
            _: Environment,
            credential: Option<&'a Secret>,
        ) -> BoxFuture<'a, Result<Vec<Capability>, ProviderError>> {
            Box::pin(async move {
                match credential.map(Secret::expose) {
                    Some("good_key") => Ok(vec![Capability::PostalSendLetter]),
                    Some(_) => Err(ProviderError::Unauthorized),
                    None => Err(ProviderError::CredentialRequired),
                }
            })
        }

        fn quote<'a>(&'a self, _: &'a MailItem) -> BoxFuture<'a, Result<Price, ProviderError>> {
            unimplemented!("not used by admin tests")
        }

        fn submit<'a>(
            &'a self,
            _: &'a AuthorizedOperation,
            _: Account<'a>,
            _: &'a MailItem,
            _: &'a [Document],
        ) -> BoxFuture<'a, Result<Submission, SubmitError>> {
            unimplemented!("not used by admin tests")
        }

        fn lookup<'a>(
            &'a self,
            _: &'a str,
            _: Account<'a>,
        ) -> BoxFuture<'a, Result<Lookup, ProviderError>> {
            unimplemented!("not used by admin tests")
        }

        fn track<'a>(
            &'a self,
            _: &'a str,
            _: Account<'a>,
        ) -> BoxFuture<'a, Result<Tracking, ProviderError>> {
            unimplemented!("not used by admin tests")
        }
    }

    struct Fixture {
        store: Store,
        providers: Registry,
        secrets: MemoryStore,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                store: Store::open_in_memory().unwrap(),
                providers: Registry::new(vec![
                    Arc::new(MockProvider::default()),
                    Arc::new(KeyedProvider),
                ]),
                secrets: MemoryStore::default(),
            }
        }

        fn admin<'a>(&'a self, terminal: &'a mut Scripted) -> Admin<'a> {
            Admin {
                store: &self.store,
                providers: &self.providers,
                secrets: &self.secrets,
                terminal,
            }
        }

        async fn mock_connection(&self) -> Connection {
            self.admin(&mut Scripted::yes())
                .connect("mock", Environment::Test, None)
                .await
                .unwrap()
        }

        fn kinds(&self) -> Vec<String> {
            let mut kinds: Vec<_> = self
                .store
                .audit_events(100)
                .unwrap()
                .into_iter()
                .map(|r| r.event.kind)
                .collect();
            kinds.reverse();
            kinds
        }
    }

    fn request(connection: &Connection) -> GrantRequest {
        GrantRequest {
            principal_id: "assistant".into(),
            connection_ids: vec![connection.id.clone()],
            actions: vec![Capability::PostalSendLetter],
            constraints: Constraints {
                max_cost_per_item: Some(Money::new("USD", 300)),
                ..Constraints::default()
            },
            expires_at: None,
        }
    }

    #[tokio::test]
    async fn connect_mock_stores_a_connection_and_grants_nothing() {
        let fx = Fixture::new();
        let connection = fx.mock_connection().await;
        assert!(connection.id.starts_with("conn_mock_"));
        assert_eq!(connection.status, ConnectionStatus::Connected);
        assert_eq!(connection.credential, CredentialRef::None);
        assert_eq!(fx.store.connections().unwrap(), [connection]);
        assert!(fx.store.grants(None).unwrap().is_empty());
        assert_eq!(fx.kinds(), ["connection.created"]);
    }

    #[tokio::test]
    async fn connect_and_grant_refuse_without_a_person() {
        let fx = Fixture::new();
        let connection = fx.mock_connection().await;

        let mut absent = Scripted::default();
        let err = fx
            .admin(&mut absent)
            .connect("mock", Environment::Test, None)
            .await
            .unwrap_err();
        assert!(matches!(err, AdminError::NotInteractive));
        let err = fx
            .admin(&mut absent)
            .grant(request(&connection))
            .unwrap_err();
        assert!(matches!(err, AdminError::NotInteractive));

        let mut says_no = Scripted {
            interactive: true,
            ..Scripted::default()
        };
        let err = fx
            .admin(&mut says_no)
            .grant(request(&connection))
            .unwrap_err();
        assert!(matches!(err, AdminError::Declined));

        assert_eq!(fx.store.connections().unwrap().len(), 1);
        assert!(fx.store.grants(None).unwrap().is_empty());
    }

    #[tokio::test]
    async fn api_key_is_verified_then_kept_only_in_the_secret_store() {
        let fx = Fixture::new();
        let mut terminal = Scripted {
            secret: Some("good_key"),
            ..Scripted::yes()
        };
        let connection = fx
            .admin(&mut terminal)
            .connect("keyed", Environment::Test, None)
            .await
            .unwrap();
        assert_eq!(connection.credential, CredentialRef::Keyring);
        assert_eq!(connection.capabilities, [Capability::PostalSendLetter]);
        let stored = fx.secrets.get(&connection.id).unwrap().unwrap();
        assert_eq!(stored.expose(), "good_key");
        for record in fx.store.audit_events(10).unwrap() {
            assert!(!format!("{record:?}").contains("good_key"));
        }

        // Disconnecting deletes the credential and keeps the record.
        fx.admin(&mut Scripted::default())
            .disconnect(&connection.id)
            .unwrap();
        assert_eq!(fx.secrets.get(&connection.id).unwrap(), None);
        let after = fx.store.connection(&connection.id).unwrap().unwrap();
        assert_eq!(after.status, ConnectionStatus::Disconnected);
    }

    #[tokio::test]
    async fn rejected_key_stores_nothing() {
        let fx = Fixture::new();
        let mut terminal = Scripted {
            secret: Some("bad_key"),
            ..Scripted::yes()
        };
        let err = fx
            .admin(&mut terminal)
            .connect("keyed", Environment::Test, None)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AdminError::Provider(ProviderError::Unauthorized)
        ));
        assert!(fx.store.connections().unwrap().is_empty());
        assert!(fx.kinds().is_empty());
    }

    #[tokio::test]
    async fn connect_validates_provider_and_environment() {
        let fx = Fixture::new();
        for (provider, environment) in [("nope", Environment::Test), ("mock", Environment::Live)] {
            let err = fx
                .admin(&mut Scripted::yes())
                .connect(provider, environment, None)
                .await
                .unwrap_err();
            assert!(matches!(err, AdminError::Invalid(_)), "{err}");
        }
    }

    #[tokio::test]
    async fn grant_shows_every_term_before_storing() {
        let fx = Fixture::new();
        let connection = fx.mock_connection().await;
        let mut terminal = Scripted::yes();
        let grant = fx.admin(&mut terminal).grant(request(&connection)).unwrap();

        let shown = terminal.shown.last().unwrap();
        for term in ["assistant", &connection.id, "postal.send.letter", "300 USD"] {
            assert!(shown.contains(term), "{term} missing from:\n{shown}");
        }
        assert!(shown.contains("unattended"));
        assert_eq!(fx.store.grants(Some("assistant")).unwrap(), [grant]);
        assert_eq!(fx.kinds(), ["connection.created", "grant.created"]);
    }

    #[tokio::test]
    async fn grant_rejects_what_it_cannot_honor() {
        let fx = Fixture::new();
        let connection = fx.mock_connection().await;
        let base = request(&connection);
        let bad: Vec<GrantRequest> = vec![
            GrantRequest {
                principal_id: "  ".into(),
                ..base.clone()
            },
            GrantRequest {
                actions: vec![],
                ..base.clone()
            },
            GrantRequest {
                connection_ids: vec!["conn_nope".into()],
                ..base.clone()
            },
            GrantRequest {
                expires_at: Some(clock::now() - time::Duration::hours(1)),
                ..base.clone()
            },
            GrantRequest {
                constraints: Constraints {
                    monthly_budget: Some(Money::new("USD", -1)),
                    ..Constraints::default()
                },
                ..base.clone()
            },
        ];
        for request in bad {
            let err = fx.admin(&mut Scripted::yes()).grant(request).unwrap_err();
            assert!(matches!(err, AdminError::Invalid(_)), "{err}");
        }

        // An action the account lacks, and a disconnected account.
        let mut terminal = Scripted {
            secret: Some("good_key"),
            ..Scripted::yes()
        };
        let keyed = fx
            .admin(&mut terminal)
            .connect("keyed", Environment::Test, None)
            .await
            .unwrap();
        let mut too_much = request(&keyed);
        too_much.actions = vec![Capability::FaxSend];
        let err = fx.admin(&mut Scripted::yes()).grant(too_much).unwrap_err();
        assert!(
            err.to_string().contains("does not support fax.send"),
            "{err}"
        );

        fx.admin(&mut Scripted::yes())
            .disconnect(&keyed.id)
            .unwrap();
        let err = fx
            .admin(&mut Scripted::yes())
            .grant(request(&keyed))
            .unwrap_err();
        assert!(err.to_string().contains("not connected"), "{err}");
        assert!(fx.store.grants(None).unwrap().is_empty());
    }

    #[tokio::test]
    async fn revoke_needs_no_confirmation_and_happens_once() {
        let fx = Fixture::new();
        let connection = fx.mock_connection().await;
        let grant = fx
            .admin(&mut Scripted::yes())
            .grant(request(&connection))
            .unwrap();

        let mut absent = Scripted::default();
        fx.admin(&mut absent).revoke(&grant.id).unwrap();
        assert!(
            fx.store
                .grant(&grant.id)
                .unwrap()
                .unwrap()
                .revoked_at
                .is_some()
        );
        assert!(fx.admin(&mut absent).revoke(&grant.id).is_err());
        assert!(fx.admin(&mut absent).revoke("grant_nope").is_err());
        assert_eq!(fx.kinds().last().unwrap(), "grant.revoked");
    }
}
