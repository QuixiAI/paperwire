//! Command-line front end for [`Admin`].

use std::io::Write;

use clap::{Args, Subcommand};

use super::{Admin, AdminError, GrantRequest, describe_grant};
use crate::core::{Capability, Money, Timestamp, clock};
use crate::delegation::Constraints;
use crate::operations::ProviderPrice;
use crate::providers::Environment;
use crate::store::enum_text;

#[derive(Subcommand)]
pub enum AdminCommand {
    /// Connect a provider account. Asks for confirmation on the terminal.
    Connect {
        /// Provider id, e.g. "mock".
        provider: String,
        /// "test" or "live".
        #[arg(long = "env", default_value = "test", value_parser = parse_environment)]
        environment: Environment,
        /// Read the API key from this environment variable at use time
        /// instead of storing it in the OS credential store.
        #[arg(long, value_name = "VARIABLE")]
        from_env: Option<String>,
    },
    /// List connected accounts.
    Connections,
    /// Disconnect an account and delete its stored credential.
    Disconnect { connection_id: String },
    /// Grant an AI client permissions. Asks for confirmation on the terminal.
    Grant(GrantArgs),
    /// List grants.
    Grants {
        /// Only grants for this client.
        #[arg(long)]
        client: Option<String>,
    },
    /// Revoke a grant. Takes effect immediately.
    Revoke { grant_id: String },
    /// Set your price for a provider product, for providers that cannot
    /// quote (Lob). Asks for confirmation on the terminal.
    Price {
        /// Provider id, e.g. "lob".
        provider: String,
        /// Product, e.g. "letter".
        product: String,
        /// Price of an item with one billable page, in minor units:
        /// USD:120 is $1.20.
        #[arg(long, value_name = "CURRENCY:MINOR", value_parser = parse_money)]
        base: Money,
        /// Added for each further billable page, in minor units.
        #[arg(long, value_name = "MINOR", default_value_t = 0)]
        per_extra_page: i64,
    },
    /// List the prices you have set.
    Prices,
    /// List sent items, newest first.
    Operations {
        /// Only operations of this client.
        #[arg(long)]
        client: Option<String>,
    },
    /// Settle an operation whose outcome is uncertain, after checking with
    /// the provider yourself. Asks for confirmation on the terminal.
    Resolve {
        operation_id: String,
        /// What the provider says happened: "accepted" or "failed".
        #[arg(value_parser = ["accepted", "failed"])]
        outcome: String,
    },
    /// Show recent audit events, newest first.
    Audit {
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
}

#[derive(Args)]
pub struct GrantArgs {
    /// The client name the server was started with (`serve --client`).
    #[arg(long)]
    client: String,
    /// Connection the grant applies to. Repeatable.
    #[arg(long = "connection", required = true, value_name = "CONNECTION_ID")]
    connections: Vec<String>,
    /// Capability to allow, e.g. postal.send.letter. Repeatable.
    #[arg(long = "action", required = true, value_name = "CAPABILITY")]
    actions: Vec<Capability>,
    /// Allowed destination country (ISO alpha-2). Repeatable.
    #[arg(long = "country", value_name = "CODE")]
    countries: Vec<String>,
    #[arg(long, value_name = "PAGES")]
    max_pages: Option<u32>,
    /// Most one item may cost, in minor units: USD:300 is $3.00.
    #[arg(long, value_name = "CURRENCY:MINOR", value_parser = parse_money)]
    max_cost: Option<Money>,
    /// Most the client may spend per UTC month, in minor units.
    #[arg(long, value_name = "CURRENCY:MINOR", value_parser = parse_money)]
    monthly_budget: Option<Money>,
    #[arg(long, value_name = "ITEMS")]
    max_items_per_day: Option<u32>,
    /// Allowed provider product, e.g. letter. Repeatable.
    #[arg(long = "product", value_name = "PRODUCT")]
    products: Vec<String>,
    /// Require your approval for each operation.
    #[arg(long)]
    require_approval: bool,
    /// Expiry as an RFC 3339 time, e.g. 2027-01-01T00:00:00Z.
    #[arg(long, value_name = "TIME", value_parser = parse_time, conflicts_with = "expires_in")]
    expires: Option<Timestamp>,
    /// Expiry from now, e.g. 30d or 12h.
    #[arg(long, value_name = "SPAN", value_parser = parse_span)]
    expires_in: Option<time::Duration>,
}

fn parse_environment(s: &str) -> Result<Environment, String> {
    match s {
        "test" => Ok(Environment::Test),
        "live" => Ok(Environment::Live),
        _ => Err("expected \"test\" or \"live\"".into()),
    }
}

fn parse_money(s: &str) -> Result<Money, String> {
    let error = || "expected CURRENCY:MINOR_UNITS, e.g. USD:300 for $3.00".to_owned();
    let (currency, amount) = s.split_once(':').ok_or_else(error)?;
    let valid_currency = currency.len() == 3 && currency.bytes().all(|b| b.is_ascii_uppercase());
    match amount.parse::<i64>() {
        Ok(amount) if valid_currency && amount >= 0 => Ok(Money::new(currency, amount)),
        _ => Err(error()),
    }
}

fn parse_time(s: &str) -> Result<Timestamp, String> {
    clock::parse_rfc3339(s).map_err(|e| e.to_string())
}

fn parse_span(s: &str) -> Result<time::Duration, String> {
    clock::parse_span(s).ok_or_else(|| "expected a span like 30d or 12h".into())
}

fn non_empty(values: Vec<String>) -> Option<Vec<String>> {
    (!values.is_empty()).then_some(values)
}

impl GrantArgs {
    fn into_request(self) -> GrantRequest {
        GrantRequest {
            principal_id: self.client,
            connection_ids: self.connections,
            actions: self.actions,
            constraints: Constraints {
                destination_countries: non_empty(
                    self.countries
                        .iter()
                        .map(|c| c.trim().to_ascii_uppercase())
                        .collect(),
                ),
                max_pages_per_item: self.max_pages,
                max_cost_per_item: self.max_cost,
                monthly_budget: self.monthly_budget,
                max_items_per_day: self.max_items_per_day,
                allowed_products: non_empty(self.products),
                require_additional_approval: self.require_approval,
            },
            expires_at: self
                .expires
                .or_else(|| self.expires_in.map(|span| clock::now() + span)),
        }
    }
}

/// Run one admin command, writing its result to `out`.
pub async fn run(
    command: AdminCommand,
    admin: &mut Admin<'_>,
    out: &mut dyn Write,
) -> Result<(), AdminError> {
    match command {
        AdminCommand::Connect {
            provider,
            environment,
            from_env,
        } => {
            let connection = admin.connect(&provider, environment, from_env).await?;
            writeln!(out, "Connected: {}", connection.id)?;
            writeln!(
                out,
                "The AI has no permissions on it yet. Grant some with:\n  \
                 postal-mcp admin grant --client default --connection {} --action <capability>",
                connection.id
            )?;
        }
        AdminCommand::Connections => {
            let connections = admin.connections()?;
            if connections.is_empty() {
                writeln!(out, "No connections.")?;
            }
            for c in connections {
                writeln!(
                    out,
                    "{}  {} ({})  {}",
                    c.id,
                    c.provider,
                    enum_text(&c.environment),
                    enum_text(&c.status)
                )?;
            }
        }
        AdminCommand::Disconnect { connection_id } => {
            admin.disconnect(&connection_id)?;
            writeln!(out, "Disconnected: {connection_id}")?;
        }
        AdminCommand::Grant(args) => {
            let grant = admin.grant(args.into_request())?;
            writeln!(out, "Granted: {}", grant.id)?;
        }
        AdminCommand::Grants { client } => {
            let grants = admin.grants(client.as_deref())?;
            if grants.is_empty() {
                writeln!(out, "No grants.")?;
            }
            let now = clock::now();
            for g in grants {
                let state = if let Some(at) = g.revoked_at {
                    format!("revoked {}", clock::to_rfc3339(at))
                } else if g.is_expired(now) {
                    "expired".into()
                } else {
                    "active".into()
                };
                writeln!(out, "{}  [{state}]\n{}\n", g.id, describe_grant(&g))?;
            }
        }
        AdminCommand::Revoke { grant_id } => {
            admin.revoke(&grant_id)?;
            writeln!(out, "Revoked: {grant_id}")?;
        }
        AdminCommand::Price {
            provider,
            product,
            base,
            per_extra_page,
        } => {
            admin.set_price(ProviderPrice {
                provider: provider.clone(),
                product: product.clone(),
                base,
                per_extra_page_minor: per_extra_page,
            })?;
            writeln!(out, "Price set: {provider} {product}")?;
        }
        AdminCommand::Prices => {
            let prices = admin.prices()?;
            if prices.is_empty() {
                writeln!(out, "No prices set.")?;
            }
            for p in prices {
                writeln!(
                    out,
                    "{} {}: {} {} + {} per extra page",
                    p.provider,
                    p.product,
                    p.base.amount_minor,
                    p.base.currency,
                    p.per_extra_page_minor
                )?;
            }
        }
        AdminCommand::Operations { client } => {
            let operations = admin.operations(client.as_deref())?;
            if operations.is_empty() {
                writeln!(out, "No operations.")?;
            }
            for o in operations {
                writeln!(
                    out,
                    "{}  {}  {}  client={}  connection={}  {}{}",
                    o.id,
                    clock::to_rfc3339(o.created_at),
                    enum_text(&o.status),
                    o.principal_id,
                    o.connection_id,
                    o.action,
                    o.provider_reference
                        .map(|r| format!("  reference={r}"))
                        .unwrap_or_default(),
                )?;
            }
        }
        AdminCommand::Resolve {
            operation_id,
            outcome,
        } => {
            admin.resolve(&operation_id, outcome == "accepted")?;
            writeln!(out, "Resolved: {operation_id} is {outcome}")?;
        }
        AdminCommand::Audit { limit } => {
            for record in admin.audit(limit)? {
                let e = record.event;
                let mut line = format!("{}  {}", clock::to_rfc3339(record.at), e.kind);
                for (label, value) in [
                    ("client", e.principal_id),
                    ("connection", e.connection_id),
                    ("grant", e.grant_id),
                    ("action", e.action),
                    ("decision", e.decision),
                    ("reason", e.reason),
                    ("detail", e.detail.filter(|d| !d.contains('\n'))),
                ] {
                    if let Some(value) = value {
                        line.push_str(&format!("  {label}={value}"));
                    }
                }
                writeln!(out, "{line}")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_is_currency_and_non_negative_minor_units() {
        assert_eq!(parse_money("USD:300"), Ok(Money::new("USD", 300)));
        for bad in ["300", "usd:300", "USD:3.00", "USD:-1", "DOLLARS:1", "USD:"] {
            assert!(parse_money(bad).is_err(), "{bad}");
        }
    }
}
