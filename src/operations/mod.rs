//! Sending (design.md §13, §19): quote, authorize, submit, and settle.
//!
//! The order of steps in [`Operations::send`] is the reliability design:
//!
//! 1. In one transaction: evaluate authority, reserve the cost, and record
//!    the operation as `pending`. Nothing has left the process yet.
//! 2. Submit to the provider, with the operation id as idempotency key.
//! 3. Record the outcome. Accepted commits the reservation, a definite
//!    rejection releases it, and an uncertain outcome keeps it held until
//!    it is reconciled (§16.4, §19.2).

use std::sync::Arc;

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use time::Duration;

use crate::connections::{Connection, SecretStore, resolve};
use crate::core::{Capability, Money, Timestamp, clock, new_id};
use crate::delegation::{self, Evaluation, OperationContext};
use crate::documents::{self, Document, DocumentFormat};
use crate::providers::{
    Account, Lookup, ProviderAdapter, ProviderError, Registry, Submission, SubmitError, Tracking,
};
use crate::store::{AuditEvent, Store, StoreError, Tx, enum_text};

/// How long a quote can be sent against.
pub const QUOTE_LIFETIME: Duration = Duration::minutes(15);

/// A `pending` operation older than this was interrupted before its outcome
/// was recorded (the process died mid-send), so it is treated as uncertain.
/// Longer than any submission can take.
pub const PENDING_STALE_AFTER: Duration = Duration::minutes(5);

/// A postal address. Normalized before any policy decision (§16.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Address {
    pub name: String,
    pub line1: String,
    pub line2: Option<String>,
    pub city: String,
    /// State, province or region, where the country uses one.
    pub state: Option<String>,
    pub postal_code: String,
    /// ISO 3166-1 alpha-2 country code, e.g. "US".
    pub country: String,
}

impl Address {
    /// Trimmed, with an upper-case country code, or what is wrong with it.
    pub fn normalized(self) -> Result<Self, String> {
        let required = |field: &str, value: String| {
            let value = value.trim().to_owned();
            if value.is_empty() {
                Err(format!("address {field} is required"))
            } else {
                Ok(value)
            }
        };
        let optional =
            |value: Option<String>| value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let country = self.country.trim().to_ascii_uppercase();
        if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err("address country must be a two-letter ISO code, e.g. US".into());
        }
        Ok(Self {
            name: required("name", self.name)?,
            line1: required("line1", self.line1)?,
            line2: optional(self.line2),
            city: required("city", self.city)?,
            state: optional(self.state),
            postal_code: required("postal_code", self.postal_code)?,
            country,
        })
    }
}

/// A document as it was when the item was quoted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemDocument {
    pub document_id: String,
    #[serde(default)]
    pub format: DocumentFormat,
    pub sha256: String,
    pub page_count: u32,
}

/// One piece of mail: the material terms a quote and its operation are
/// bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailItem {
    /// Provider product, e.g. "letter".
    pub product: String,
    pub to: Address,
    pub from: Option<Address>,
    /// In the order they are printed (§12.3).
    pub documents: Vec<ItemDocument>,
    pub page_count: u32,
}

impl MailItem {
    /// What the policy engine checks this item against.
    pub fn context(&self, price: &Price) -> OperationContext {
        OperationContext {
            destination_country: Some(self.to.country.clone()),
            page_count: Some(self.page_count),
            product: Some(self.product.clone()),
            max_cost: price.amount.clone(),
        }
    }
}

/// How far a price can be relied on (§13.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PriceKind {
    /// The provider will charge exactly this.
    Firm,
    /// An upper bound; the charge may be lower.
    Estimated,
    /// The provider cannot price the item in advance.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Price {
    pub kind: PriceKind,
    /// Absent exactly when the kind is `unavailable`.
    pub amount: Option<Money>,
}

/// The user's own price for a provider product, for providers that cannot
/// quote (design.md §13.4). It is what bounds the cost of a send, so only
/// the control plane sets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderPrice {
    pub provider: String,
    pub product: String,
    /// The price of an item with one billable page.
    pub base: Money,
    /// Added for each further billable page, in the same currency.
    pub per_extra_page_minor: i64,
}

impl ProviderPrice {
    /// The price of an item with `pages` billable pages, or `None` on
    /// overflow.
    pub fn for_pages(&self, pages: u32) -> Option<Money> {
        let extra = i64::from(pages.saturating_sub(1)).checked_mul(self.per_extra_page_minor)?;
        let amount = self.base.amount_minor.checked_add(extra)?;
        Some(Money::new(self.base.currency.clone(), amount))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    pub id: String,
    pub principal_id: String,
    pub connection_id: String,
    pub action: Capability,
    pub item: MailItem,
    pub price: Price,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
}

/// Where an operation stands with the provider. Acceptance is not delivery
/// (§19.5): later stages come from the provider's own tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    /// Authorized and recorded; no outcome recorded yet.
    Pending,
    /// The provider accepted the order.
    Accepted,
    /// The provider did not accept the order. Nothing was charged.
    Failed,
    /// The provider may or may not have accepted the order.
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    pub id: String,
    pub quote_id: String,
    pub principal_id: String,
    pub connection_id: String,
    pub grant_id: String,
    pub policy_version: u32,
    pub action: Capability,
    pub status: OperationStatus,
    pub provider_reference: Option<String>,
    pub error: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct QuoteRequest {
    pub connection_id: String,
    pub product: String,
    pub to: Address,
    pub from: Option<Address>,
    pub document_ids: Vec<String>,
}

#[derive(Debug)]
pub enum SendOutcome {
    /// The operation, in whatever state it reached. Sending a quote again
    /// returns the same operation without contacting the provider.
    Operation(Operation),
    /// Not permitted. Nothing was recorded, reserved or sent.
    Denied(Evaluation),
}

#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    /// The request itself is wrong; the message says how.
    #[error("{0}")]
    Invalid(String),
    #[error("the provider could not price this item: {0}")]
    Provider(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn invalid<T>(message: impl Into<String>) -> Result<T, OperationError> {
    Err(OperationError::Invalid(message.into()))
}

/// The capability a product needs.
fn action_for(product: &str) -> Result<Capability, OperationError> {
    match product {
        "letter" => Ok(Capability::PostalSendLetter),
        other => invalid(format!(
            "unsupported product {other:?}; supported: \"letter\""
        )),
    }
}

/// How a submission ended.
enum Outcome {
    Accepted(Submission),
    Failed(String),
    Uncertain(String),
}

#[derive(Clone)]
pub struct Operations {
    store: Store,
    providers: Registry,
    secrets: Arc<dyn SecretStore>,
}

impl Operations {
    pub fn new(store: Store, providers: Registry, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            store,
            providers,
            secrets,
        }
    }

    /// Price an item and say whether sending it would be permitted now.
    /// Nothing is reserved; authority is decided again at send time.
    pub async fn quote(
        &self,
        principal: &str,
        request: QuoteRequest,
    ) -> Result<(Quote, Evaluation), OperationError> {
        let now = clock::now();
        let action = action_for(&request.product)?;
        let Some(connection) = delegation::visible_connections(&self.store, principal, now)?
            .into_iter()
            .find(|c| c.id == request.connection_id)
        else {
            return invalid(format!(
                "no accessible connection {:?}; call connections_list",
                request.connection_id
            ));
        };
        let Some(adapter) = self.providers.get(&connection.provider) else {
            return invalid(format!("provider {} is not available", connection.provider));
        };

        if request.document_ids.is_empty() {
            return invalid("at least one document is required; call documents_prepare");
        }
        let mut item_documents = Vec::new();
        for id in &request.document_ids {
            let Some(document) = documents::get(&self.store, principal, id)? else {
                return invalid(format!("no document {id:?}; call documents_prepare"));
            };
            item_documents.push(ItemDocument {
                document_id: document.id,
                format: document.format,
                sha256: document.sha256,
                page_count: document.page_count,
            });
        }
        let item = MailItem {
            product: request.product,
            to: request.to.normalized().map_err(OperationError::Invalid)?,
            from: request
                .from
                .map(Address::normalized)
                .transpose()
                .map_err(OperationError::Invalid)?,
            page_count: item_documents.iter().map(|d| d.page_count).sum(),
            documents: item_documents,
        };

        let mut price = adapter.quote(&item).await.map_err(|e| match e {
            ProviderError::InvalidItem(reason) => OperationError::Invalid(reason),
            other => OperationError::Provider(other.to_string()),
        })?;
        // A provider that cannot quote is priced from the user's own table,
        // as an estimate. With no entry the price stays unavailable, and
        // the item cannot be sent: prices are never invented (§13.4).
        if price.kind == PriceKind::Unavailable
            && let Some(table) = self.store.price(&connection.provider, &item.product)?
            && let Some(amount) = table.for_pages(adapter.billable_pages(&item))
        {
            price = Price {
                kind: PriceKind::Estimated,
                amount: Some(amount),
            };
        }
        let quote = Quote {
            id: new_id("quote"),
            principal_id: principal.to_owned(),
            connection_id: connection.id,
            action,
            item,
            price,
            created_at: now,
            expires_at: now + QUOTE_LIFETIME,
        };
        self.store.insert_quote(&quote)?;
        let evaluation = delegation::check(
            &self.store,
            principal,
            &quote.connection_id,
            action,
            &quote.item.context(&quote.price),
            now,
        )?;
        Ok((quote, evaluation))
    }

    /// Send what `quote_id` describes, if `principal` is allowed to.
    pub async fn send(
        &self,
        principal: &str,
        quote_id: &str,
    ) -> Result<SendOutcome, OperationError> {
        let now = clock::now();
        let Some(quote) = self
            .store
            .quote(quote_id)?
            .filter(|q| q.principal_id == principal)
        else {
            return invalid(format!("no quote {quote_id:?}; call correspondence_quote"));
        };

        // Step 1: decide, reserve and record, atomically.
        enum Begun {
            Replay(Operation),
            Expired,
            Denied(Evaluation),
            Authorized(delegation::AuthorizedOperation, Operation),
        }
        let begun = self.store.atomically(|tx| -> Result<Begun, StoreError> {
            // A quote is sent at most once: a repeat is a replay, not a
            // second letter.
            if let Some(existing) = tx.operation_for_quote(&quote.id)? {
                return Ok(Begun::Replay(existing));
            }
            if quote.expires_at <= now {
                return Ok(Begun::Expired);
            }
            let operation_id = new_id("op");
            let authorized = match delegation::authorize(
                tx,
                &operation_id,
                principal,
                &quote.connection_id,
                quote.action,
                &quote.item.context(&quote.price),
                now,
            )? {
                Ok(authorized) => authorized,
                Err(evaluation) => return Ok(Begun::Denied(evaluation)),
            };
            let operation = Operation {
                id: operation_id,
                quote_id: quote.id.clone(),
                principal_id: principal.to_owned(),
                connection_id: quote.connection_id.clone(),
                grant_id: authorized.grant_id().to_owned(),
                policy_version: authorized.policy_version(),
                action: quote.action,
                status: OperationStatus::Pending,
                provider_reference: None,
                error: None,
                created_at: now,
                updated_at: now,
            };
            tx.insert_operation(&operation)?;
            Ok(Begun::Authorized(authorized, operation))
        })?;
        let (authorized, operation) = match begun {
            Begun::Replay(operation) => return Ok(SendOutcome::Operation(operation)),
            Begun::Expired => return invalid("this quote has expired; request a new one"),
            Begun::Denied(evaluation) => return Ok(SendOutcome::Denied(evaluation)),
            Begun::Authorized(authorized, operation) => (authorized, operation),
        };

        // Step 2: submit. Anything that fails before the adapter is called
        // is a definite failure: nothing was sent.
        let outcome = match self.prepare_submission(&quote, principal) {
            Err(reason) => Outcome::Failed(reason),
            Ok((adapter, connection, documents)) => {
                match resolve(
                    &connection.credential,
                    &connection.id,
                    self.secrets.as_ref(),
                ) {
                    Err(e) => Outcome::Failed(e.to_string()),
                    Ok(credential) => {
                        let account = Account {
                            environment: connection.environment,
                            credential: credential.as_ref(),
                        };
                        match adapter
                            .submit(&authorized, account, &quote.item, &documents)
                            .await
                        {
                            Ok(submission) => Outcome::Accepted(submission),
                            Err(SubmitError::Rejected(reason)) => Outcome::Failed(reason),
                            Err(SubmitError::Uncertain(reason)) => Outcome::Uncertain(reason),
                        }
                    }
                }
            }
        };

        // Step 3: record the outcome.
        self.settle(&operation, authorized.max_cost(), outcome)?;
        Ok(SendOutcome::Operation(self.reload(&operation.id)?))
    }

    /// The operation, if it is `principal`'s. An unsettled operation is
    /// first reconciled with the provider.
    pub async fn status(
        &self,
        principal: &str,
        operation_id: &str,
    ) -> Result<Operation, OperationError> {
        let Some(operation) = self
            .store
            .operation(operation_id)?
            .filter(|o| o.principal_id == principal)
        else {
            return invalid(format!("no operation {operation_id:?}"));
        };
        let unsettled = match operation.status {
            OperationStatus::Uncertain => true,
            OperationStatus::Pending => operation.updated_at + PENDING_STALE_AFTER <= clock::now(),
            OperationStatus::Accepted | OperationStatus::Failed => false,
        };
        if unsettled {
            self.reconcile(&operation).await?;
        }
        self.reload(operation_id)
    }

    /// The provider's delivery report for an accepted operation, if this
    /// client may read status on the connection (`postal.read.status`).
    /// `None` when it is not accepted, not permitted, or the provider
    /// cannot be reached: tracking is extra information, never an error.
    pub async fn track(
        &self,
        principal: &str,
        operation: &Operation,
    ) -> Result<Option<Tracking>, OperationError> {
        let (OperationStatus::Accepted, Some(reference)) =
            (operation.status, operation.provider_reference.as_deref())
        else {
            return Ok(None);
        };
        let permitted = delegation::check(
            &self.store,
            principal,
            &operation.connection_id,
            Capability::PostalReadStatus,
            &OperationContext::default(),
            clock::now(),
        )?;
        if !permitted.is_allowed() {
            return Ok(None);
        }
        let Some(connection) = self.store.connection(&operation.connection_id)? else {
            return Ok(None);
        };
        let Some(adapter) = self.providers.get(&connection.provider) else {
            return Ok(None);
        };
        let Ok(credential) = resolve(
            &connection.credential,
            &connection.id,
            self.secrets.as_ref(),
        ) else {
            return Ok(None);
        };
        let account = Account {
            environment: connection.environment,
            credential: credential.as_ref(),
        };
        Ok(adapter.track(reference, account).await.ok())
    }

    /// Ask the provider what became of an unsettled operation (§19.2). The
    /// operation is never resubmitted here. If the provider cannot say, it
    /// stays uncertain and its reservation stays held.
    async fn reconcile(&self, operation: &Operation) -> Result<(), OperationError> {
        let Some(quote) = self.store.quote(&operation.quote_id)? else {
            return Ok(());
        };
        let Some(connection) = self.store.connection(&operation.connection_id)? else {
            return Ok(());
        };
        let Some(adapter) = self.providers.get(&connection.provider) else {
            return Ok(());
        };
        let Ok(credential) = resolve(
            &connection.credential,
            &connection.id,
            self.secrets.as_ref(),
        ) else {
            return Ok(());
        };
        let account = Account {
            environment: connection.environment,
            credential: credential.as_ref(),
        };
        let outcome = match adapter.lookup(&operation.id, account).await {
            Ok(Lookup::Found(submission)) => Outcome::Accepted(submission),
            Ok(Lookup::NotFound) => {
                Outcome::Failed("the provider has no record of this operation".into())
            }
            Ok(Lookup::Unknown) | Err(_) => return Ok(()),
        };
        let Some(reserved) = quote.price.amount else {
            return Ok(());
        };
        self.settle(operation, &reserved, outcome)?;
        Ok(())
    }

    /// Everything a submission needs, or why it cannot start.
    #[allow(clippy::type_complexity)]
    fn prepare_submission(
        &self,
        quote: &Quote,
        principal: &str,
    ) -> Result<(Arc<dyn ProviderAdapter>, Connection, Vec<Document>), String> {
        let storage = |e: StoreError| {
            tracing::error!(error = %e, "storage error");
            "internal storage error".to_owned()
        };
        let connection = self
            .store
            .connection(&quote.connection_id)
            .map_err(storage)?
            .ok_or("the connection no longer exists")?;
        let adapter = self
            .providers
            .get(&connection.provider)
            .ok_or("the provider is not available")?;
        let mut documents = Vec::new();
        for quoted in &quote.item.documents {
            let document = documents::get(&self.store, principal, &quoted.document_id)
                .map_err(storage)?
                .ok_or("a document no longer exists")?;
            // What is sent must be what was quoted and authorized.
            if document.sha256 != quoted.sha256 {
                return Err("a document changed after it was quoted".into());
            }
            documents.push(document);
        }
        Ok((adapter, connection, documents))
    }

    /// Record how an operation ended and settle its reservation, once.
    fn settle(
        &self,
        operation: &Operation,
        reserved: &Money,
        outcome: Outcome,
    ) -> Result<(), StoreError> {
        let now = clock::now();
        self.store.atomically(|tx| {
            let (status, reference, error) = match &outcome {
                Outcome::Accepted(s) => {
                    (OperationStatus::Accepted, Some(s.reference.as_str()), None)
                }
                Outcome::Failed(reason) => (OperationStatus::Failed, None, Some(reason.as_str())),
                Outcome::Uncertain(reason) => {
                    (OperationStatus::Uncertain, None, Some(reason.as_str()))
                }
            };
            if !tx.settle_operation(&operation.id, status, reference, error, now)? {
                // Already settled by another call; that outcome stands.
                return Ok(());
            }
            match &outcome {
                Outcome::Accepted(submission) => {
                    // Without a stated charge, count the full reservation.
                    let actual = submission.charged.as_ref().unwrap_or(reserved);
                    tx.commit_reservation(&operation.id, actual, now)?;
                }
                Outcome::Failed(_) => tx.release_reservation(&operation.id)?,
                // Uncertain: the reservation stays held (§16.4).
                Outcome::Uncertain(_) => {}
            }
            audit_operation(tx, operation, status, error, now)
        })
    }

    fn reload(&self, operation_id: &str) -> Result<Operation, OperationError> {
        self.store
            .operation(operation_id)?
            .ok_or_else(|| StoreError::Corrupt("operation").into())
    }
}

fn audit_operation(
    tx: &Tx<'_>,
    operation: &Operation,
    status: OperationStatus,
    detail: Option<&str>,
    at: Timestamp,
) -> Result<(), StoreError> {
    tx.audit(
        &AuditEvent {
            kind: format!("operation.{}", enum_text(&status)),
            principal_id: Some(operation.principal_id.clone()),
            connection_id: Some(operation.connection_id.clone()),
            grant_id: Some(operation.grant_id.clone()),
            action: Some(operation.action.to_string()),
            policy_version: Some(operation.policy_version),
            detail: Some(match detail {
                Some(detail) => format!("{}: {detail}", operation.id),
                None => operation.id.clone(),
            }),
            ..AuditEvent::default()
        },
        at,
    )
}

/// Settle an operation by hand when the provider cannot say what happened
/// (§19.2 step 5). For the control plane only.
pub fn resolve_by_operator(
    store: &Store,
    operation_id: &str,
    accepted: bool,
) -> Result<bool, StoreError> {
    let now = clock::now();
    store.atomically(|tx| {
        let Some(operation) = tx.operation(operation_id)? else {
            return Ok(false);
        };
        let (status, note) = if accepted {
            (OperationStatus::Accepted, "marked accepted by the operator")
        } else {
            (OperationStatus::Failed, "marked failed by the operator")
        };
        if !tx.settle_operation(operation_id, status, None, Some(note), now)? {
            return Ok(false);
        }
        if accepted {
            if let Some(reserved) = tx.quote(&operation.quote_id)?.and_then(|q| q.price.amount) {
                tx.commit_reservation(operation_id, &reserved, now)?;
            }
        } else {
            tx.release_reservation(operation_id)?;
        }
        audit_operation(tx, &operation, status, Some(note), now)?;
        Ok(true)
    })
}
