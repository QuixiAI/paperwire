//! `paperwire login`: connect an account and let an AI client use it, in
//! one guided step.
//!
//! It asks its questions, verifies the credential with the provider, and
//! then shows everything it is about to store (the connection, the price
//! and the grant) for a single confirmation. Nothing is stored before that.
//! Like every command that widens what the AI may do, it needs a person at
//! an interactive terminal (design.md §6.2).

use super::{
    Admin, AdminError, DEFAULT_CLIENT, GrantRequest, describe_grant, describe_price, price_line,
};
use crate::connections::{Connection, ConnectionStatus, CredentialRef};
use crate::core::{Capability, Money, Rate, Secret, clock, new_id};
use crate::delegation::{Constraints, Grant};
use crate::operations::ProviderPrice;
use crate::providers::{AuthMethod, Environment, ProviderAdapter, ProviderInfo};
use crate::store::enum_text;

/// The limits `login` suggests: letters within the US, $3 each, $30 a month.
const CURRENCY: &str = "USD";
const PRODUCT: &str = "letter";
const DEFAULT_MAX_COST: &str = "3.00";
const DEFAULT_MONTHLY_BUDGET: &str = "30.00";

#[derive(Debug, Default)]
pub struct LoginRequest {
    /// Provider id; asked for when absent.
    pub provider: Option<String>,
    /// The `serve --client` name to grant to. Absent means the name every
    /// server uses unless told otherwise.
    pub client: Option<String>,
}

#[derive(Debug)]
pub struct LoginOutcome {
    pub connection: Connection,
    /// False when an existing connection was reused.
    pub connected_now: bool,
    pub grant: Grant,
    /// The price set in this run, if any.
    pub price: Option<ProviderPrice>,
    /// True when the provider cannot price and the user has set no price:
    /// nothing can be sent until they do.
    pub price_missing: bool,
}

fn format_amount(minor: i64) -> String {
    format!("{}.{:02}", minor / 100, minor % 100)
}

impl Admin<'_> {
    /// Ask for a unit price until the answer parses. Every digit is kept.
    /// An empty answer with no default is `None`.
    fn ask_rate(
        &mut self,
        question: &str,
        default: Option<&str>,
    ) -> Result<Option<Rate>, AdminError> {
        loop {
            let answer = self.terminal.ask(question, default)?;
            if answer.is_empty() {
                return Ok(None);
            }
            match Rate::parse_major(&answer) {
                Some(rate) => return Ok(Some(rate)),
                None => self.note("Enter a price in dollars, like 1.066 or 3.")?,
            }
        }
    }

    /// Ask for a spending limit, in whole cents, until the answer parses.
    fn ask_limit(
        &mut self,
        question: &str,
        default: Option<&str>,
    ) -> Result<Option<i64>, AdminError> {
        let Some(rate) = self.ask_rate(question, default)? else {
            return Ok(None);
        };
        let limit = rate.floor_minor();
        if rate != Rate::from_minor(limit) {
            self.note(&format!(
                "Limits are in whole cents: using {}.",
                format_amount(limit)
            ))?;
        }
        Ok(Some(limit))
    }

    fn ask_yes_no(&mut self, question: &str, default: bool) -> Result<bool, AdminError> {
        let default_text = if default { "yes" } else { "no" };
        loop {
            let answer = self.terminal.ask(question, Some(default_text))?;
            match answer.to_ascii_lowercase().as_str() {
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => self.note("Answer yes or no.")?,
            }
        }
    }

    /// Tell the user something without asking anything.
    fn note(&mut self, message: &str) -> Result<(), AdminError> {
        self.terminal.note(message)
    }

    fn choose_provider(&mut self, requested: Option<String>) -> Result<String, AdminError> {
        if let Some(provider) = requested {
            return Ok(provider);
        }
        let ids: Vec<String> = self.providers.list().into_iter().map(|p| p.id).collect();
        // Suggest a real provider over the mock.
        let default = ids
            .iter()
            .find(|id| id.as_str() != "mock")
            .or(ids.first())
            .cloned()
            .ok_or_else(|| AdminError::Invalid("no providers are installed".into()))?;
        self.terminal
            .ask(&format!("Provider ({})", ids.join(", ")), Some(&default))
    }

    fn ask_environment(&mut self, info: &ProviderInfo) -> Result<Environment, AdminError> {
        if let [only] = info.environments[..] {
            return Ok(only);
        }
        loop {
            let answer = self
                .terminal
                .ask("Environment (test, live)", Some("test"))?;
            let environment = match answer.as_str() {
                "test" => Environment::Test,
                "live" => Environment::Live,
                _ => {
                    self.note("Answer test or live.")?;
                    continue;
                }
            };
            if info.environments.contains(&environment) {
                return Ok(environment);
            }
            self.note(&format!("{} has no {answer} environment.", info.name))?;
        }
    }

    /// Obtain and verify a credential. Stores nothing.
    async fn verify_new_connection(
        &mut self,
        adapter: &dyn ProviderAdapter,
        info: &ProviderInfo,
    ) -> Result<(Connection, Option<Secret>), AdminError> {
        let (credential, secret, environment) = match info.auth {
            AuthMethod::None => (CredentialRef::None, None, self.ask_environment(info)?),
            AuthMethod::ApiKey => {
                let secret = self
                    .terminal
                    .read_secret(&format!("{} API key (input hidden): ", info.name))?;
                if secret.expose().is_empty() {
                    return Err(AdminError::Invalid("no key entered".into()));
                }
                let environment = match adapter.credential_environment(&secret) {
                    Some(environment) => environment,
                    None => self.ask_environment(info)?,
                };
                (CredentialRef::Keyring, Some(secret), environment)
            }
            AuthMethod::OAuth => {
                return Err(AdminError::Invalid(
                    "OAuth connections are not supported yet".into(),
                ));
            }
        };
        let capabilities = adapter.verify(environment, secret.as_ref()).await?;
        if secret.is_some() {
            self.note(&format!(
                "{} accepted the key ({} environment).",
                info.name,
                enum_text(&environment)
            ))?;
        }
        let connection = Connection {
            id: new_id(&format!("conn_{}", info.id)),
            provider: info.id.clone(),
            environment,
            status: ConnectionStatus::Connected,
            capabilities,
            credential,
            created_at: clock::now(),
        };
        Ok((connection, secret))
    }

    /// A price for a provider that cannot quote, unless one is already set
    /// or the user skips it.
    fn ask_price(&mut self, info: &ProviderInfo) -> Result<Option<ProviderPrice>, AdminError> {
        self.note(&format!(
            "{} does not tell this server what a letter costs, and nothing is sent \
             without a known cost. Enter what {0} charges you, from your {0} pricing.",
            info.name
        ))?;
        let question = format!("Price of a letter's first page, in {CURRENCY} (blank to skip)");
        let Some(base) = self.ask_rate(&question, None)? else {
            return Ok(None);
        };
        let per_extra_page = self
            .ask_rate(
                &format!("Price of each further page, in {CURRENCY}"),
                Some("0.00"),
            )?
            .unwrap_or(Rate::ZERO);
        let price = ProviderPrice {
            provider: info.id.clone(),
            product: PRODUCT.into(),
            currency: CURRENCY.into(),
            base,
            per_extra_page,
        };
        self.check_price(&price)?;
        Ok(Some(price))
    }

    /// Connect a provider account (or reuse a connected one) and grant an
    /// AI client letters within the US under the limits the user enters.
    pub async fn login(&mut self, request: LoginRequest) -> Result<LoginOutcome, AdminError> {
        let provider = self.choose_provider(request.provider)?;
        let adapter = self
            .providers
            .get(&provider)
            .ok_or_else(|| AdminError::Invalid(format!("unknown provider {provider:?}")))?;
        let info = adapter.info();

        let existing = self
            .store
            .connections()?
            .into_iter()
            .find(|c| c.provider == info.id && c.is_ready());
        let reused = match existing {
            Some(connection)
                if self.ask_yes_no(
                    &format!(
                        "{} is already connected ({}, {} environment). Use that connection?",
                        info.name,
                        connection.id,
                        enum_text(&connection.environment)
                    ),
                    true,
                )? =>
            {
                Some(connection)
            }
            _ => None,
        };
        let (connection, secret, connected_now) = match reused {
            Some(connection) => (connection, None, false),
            None => {
                let (connection, secret) = self.verify_new_connection(&*adapter, &info).await?;
                (connection, secret, true)
            }
        };

        let actions: Vec<Capability> = [Capability::PostalSendLetter, Capability::PostalReadStatus]
            .into_iter()
            .filter(|action| connection.capabilities.contains(action))
            .collect();
        if !actions.contains(&Capability::PostalSendLetter) {
            return Err(AdminError::Invalid(format!(
                "this {} account cannot send letters; use `paperwire admin grant` \
                 to grant what it can do",
                info.name
            )));
        }

        let needs_price =
            adapter.needs_user_price() && self.store.price(&info.id, PRODUCT)?.is_none();
        let price = if needs_price {
            self.ask_price(&info)?
        } else {
            None
        };

        // The name is a label the server cannot verify, so it is not asked
        // for: every client shares one unless the user names another.
        let client = request.client.unwrap_or_else(|| DEFAULT_CLIENT.to_owned());
        let max_cost = self.ask_limit(
            &format!("Most a single letter may cost, in {CURRENCY}"),
            Some(DEFAULT_MAX_COST),
        )?;
        let monthly_budget = self.ask_limit(
            &format!("Most the AI may spend per month, in {CURRENCY}"),
            Some(DEFAULT_MONTHLY_BUDGET),
        )?;
        let grant = self.build_grant(
            GrantRequest {
                principal_id: client,
                connection_ids: vec![connection.id.clone()],
                actions,
                constraints: Constraints {
                    destination_countries: Some(vec!["US".into()]),
                    max_cost_per_item: max_cost.map(|amount| Money::new(CURRENCY, amount)),
                    monthly_budget: monthly_budget.map(|amount| Money::new(CURRENCY, amount)),
                    ..Constraints::default()
                },
                expires_at: None,
            },
            Some(&connection),
        )?;

        // One confirmation covers everything that is about to be stored.
        let mut summary = Vec::new();
        if connected_now {
            let environment = enum_text(&connection.environment);
            summary.push(format!(
                "Connect a {} account ({environment} environment).",
                info.name
            ));
            if connection.environment == Environment::Live {
                summary.push(
                    "This is a LIVE account: what the AI sends is really mailed and \
                     really charged."
                        .into(),
                );
            }
        } else {
            summary.push(format!("Use the existing connection {}.", connection.id));
        }
        if let Some(price) = &price {
            summary.push(String::new());
            summary.push(describe_price(price));
        }
        summary.push(String::new());
        summary.push(format!(
            "Grant these permissions to an AI client:\n\n{}",
            describe_grant(&grant)
        ));
        self.confirm(&summary.join("\n"))?;

        if connected_now {
            self.store_connection(&connection, secret.as_ref())?;
        }
        if let Some(price) = &price {
            self.store_price(price)?;
        }
        self.store_grant(&grant)?;

        Ok(LoginOutcome {
            connection,
            connected_now,
            grant,
            price_missing: needs_price && price.is_none(),
            price,
        })
    }
}

/// What to tell the user after a successful login.
pub fn describe_outcome(outcome: &LoginOutcome) -> String {
    let mut lines = Vec::new();
    if outcome.connected_now {
        lines.push(format!("Connected: {}", outcome.connection.id));
    }
    if let Some(price) = &outcome.price {
        lines.push(format!("Price set: {}", price_line(price)));
    }
    lines.push(format!("Granted:   {}", outcome.grant.id));
    if outcome.price_missing {
        lines.push(format!(
            "\nNo price is set, so nothing can be sent yet. Set one with:\n  \
             paperwire admin price {} {PRODUCT} --base {CURRENCY}:<cents> \
             --per-extra-page <cents>",
            outcome.connection.provider
        ));
    }
    let client = &outcome.grant.principal_id;
    let serve = if client == DEFAULT_CLIENT {
        "paperwire serve".to_owned()
    } else {
        format!("paperwire serve --client {client}")
    };
    lines.push(format!(
        "\nNow add the server to your AI client. For Claude Code:\n  \
         claude mcp add paperwire -- {serve}\n\n\
         To take the permission back: paperwire admin revoke {}",
        outcome.grant.id
    ));
    lines.join("\n")
}
