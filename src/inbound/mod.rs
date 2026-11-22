//! Received mail (design.md §14): the receiving addresses of a connected
//! mailbox account and the mail that has arrived at them.
//!
//! Everything here is a read. Nothing is opened, scanned, forwarded or
//! shredded: those are mailroom actions (§15) and are not built yet.
//!
//! Each part of an item is a separate permission. `mailbox.read.items` lets
//! a client see that mail arrived and who it is from; the envelope image
//! needs `mailbox.read.envelopes` and the scanned contents need
//! `mailbox.read.scans`. A part the client may not read is left out and
//! named in [`ReadItem::withheld`].
//!
//! What a sender wrote is untrusted (§20.4). This module passes it on as
//! data and never acts on it.

use std::sync::Arc;

use rmcp::schemars::{self, JsonSchema};
use serde::Serialize;

use crate::connections::{SecretStore, resolve};
use crate::core::{Capability, Secret, clock};
use crate::delegation::{self, Evaluation, OperationContext};
use crate::providers::{Account, Environment, ProviderAdapter, ProviderError, Registry};
use crate::store::{Store, StoreError};

/// Items returned by one inbox call when the client does not say.
pub const DEFAULT_PAGE_SIZE: u32 = 25;
/// The most items one inbox call returns.
pub const MAX_PAGE_SIZE: u32 = 100;
/// Longer cursors are refused before they reach a provider.
const MAX_CURSOR_LEN: usize = 1024;
const MAX_ID_LEN: usize = 128;

/// The street address of a receiving endpoint. It has no name line: mail is
/// addressed to the account holder at this address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct MailboxAddress {
    pub line1: String,
    pub line2: Option<String>,
    pub city: String,
    pub state: Option<String>,
    pub postal_code: String,
    /// ISO 3166-1 alpha-2 country code, e.g. "US".
    pub country: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKind {
    PostalMailbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EndpointStatus {
    /// Mail sent here is received.
    Active,
    /// The provider is not receiving mail here, e.g. the mailbox is closed
    /// or its setup is unfinished.
    Inactive,
    /// The provider reported something this server does not recognize.
    Unknown,
}

/// A provider-managed place where correspondence arrives (design.md §14.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Endpoint {
    pub endpoint_id: String,
    pub kind: EndpointKind,
    pub status: EndpointStatus,
    pub address: Option<MailboxAddress>,
    /// The provider's own wording about the endpoint's state, if any.
    pub detail: Option<String>,
}

/// Whether the contents of an item have been scanned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    /// Unopened: only the envelope is known.
    None,
    /// A scan was requested and is not finished.
    Processing,
    Completed,
}

/// The scanned contents of an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Scan {
    /// The provider's summary of the contents, if it writes one. Derived
    /// from what the sender wrote: untrusted.
    pub summary: Option<String>,
    /// Where the scanned document can be downloaded. Providers issue these
    /// for a limited time; read the item again for a fresh one.
    pub document_url: Option<String>,
}

/// One piece of received mail, as the provider reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct InboundItem {
    pub item_id: String,
    /// The endpoint it arrived at.
    pub endpoint_id: Option<String>,
    /// Who it is from, as read off the envelope. Untrusted.
    pub sender: Option<String>,
    /// Who it is addressed to, as read off the envelope.
    pub recipient: Option<String>,
    /// RFC 3339, when the provider says.
    pub received_at: Option<String>,
    /// True when this is the user's own mail coming back undelivered.
    pub returned_to_sender: bool,
    pub scan_status: ScanStatus,
    /// Where an image of the envelope can be downloaded, for a limited time.
    pub envelope_image_url: Option<String>,
    /// Present once the contents have been scanned.
    pub scan: Option<Scan>,
}

/// Which items to list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxQuery {
    /// Only items that arrived at this endpoint.
    pub endpoint_id: Option<String>,
    /// Only items received at or after this time (RFC 3339).
    pub received_after: Option<String>,
    pub limit: u32,
    /// From an earlier page's `next_cursor`.
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxPage {
    pub items: Vec<InboundItem>,
    /// Present when there are more items.
    pub next_cursor: Option<String>,
}

/// What the client asked for, before it is checked.
#[derive(Debug, Clone, Default)]
pub struct InboxRequest {
    pub connection_id: String,
    pub endpoint_id: Option<String>,
    pub received_after: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

/// An item with only the parts this client may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadItem {
    pub item: InboundItem,
    /// The permissions that would have shown more of this item.
    pub withheld: Vec<Capability>,
}

#[derive(Debug, thiserror::Error)]
pub enum InboundError {
    /// The client's own request is wrong.
    #[error("{0}")]
    Invalid(String),
    /// The policy engine did not allow the read.
    #[error("not permitted: {}", .0.message.as_deref().unwrap_or("denied"))]
    Denied(Evaluation),
    #[error("{0}")]
    Provider(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<ProviderError> for InboundError {
    fn from(e: ProviderError) -> Self {
        match e {
            ProviderError::InvalidItem(reason) => Self::Invalid(reason),
            other => Self::Provider(other.to_string()),
        }
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T, InboundError> {
    Err(InboundError::Invalid(message.into()))
}

/// Identifiers end up in provider URLs, so only plain ones are accepted:
/// letters, digits, `-` and `_`.
fn check_id(what: &str, id: &str) -> Result<(), InboundError> {
    let plain = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if plain {
        Ok(())
    } else {
        invalid(format!("{what} {id:?} is not a valid id"))
    }
}

/// A connected account a read was allowed on.
struct Allowed {
    adapter: Arc<dyn ProviderAdapter>,
    environment: Environment,
    credential: Option<Secret>,
}

impl Allowed {
    fn account(&self) -> Account<'_> {
        Account {
            environment: self.environment,
            credential: self.credential.as_ref(),
        }
    }
}

#[derive(Clone)]
pub struct Inbound {
    store: Store,
    providers: Registry,
    secrets: Arc<dyn SecretStore>,
}

impl Inbound {
    pub fn new(store: Store, providers: Registry, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            store,
            providers,
            secrets,
        }
    }

    /// The policy decision for one read, recorded like every other
    /// (design.md §6.5). State is read on each call, so a revocation
    /// applies to the next read (§6.3).
    fn evaluate(
        &self,
        principal: &str,
        connection_id: &str,
        action: Capability,
    ) -> Result<Evaluation, StoreError> {
        delegation::check(
            &self.store,
            principal,
            connection_id,
            action,
            &OperationContext::default(),
            clock::now(),
        )
    }

    /// The account behind `connection_id`, if `principal` may perform
    /// `action` on it. Nothing about the connection is revealed otherwise.
    fn allowed(
        &self,
        principal: &str,
        connection_id: &str,
        action: Capability,
    ) -> Result<Allowed, InboundError> {
        let evaluation = self.evaluate(principal, connection_id, action)?;
        if !evaluation.is_allowed() {
            return Err(InboundError::Denied(evaluation));
        }
        let unavailable = || InboundError::Provider("the connection is not available".into());
        let connection = self
            .store
            .connection(connection_id)?
            .ok_or_else(unavailable)?;
        let adapter = self
            .providers
            .get(&connection.provider)
            .ok_or_else(unavailable)?;
        let credential = resolve(
            &connection.credential,
            &connection.id,
            self.secrets.as_ref(),
        )
        .map_err(|e| InboundError::Provider(e.to_string()))?;
        Ok(Allowed {
            adapter,
            environment: connection.environment,
            credential,
        })
    }

    /// The receiving endpoints of a connection. Needs `endpoint.read`.
    pub async fn endpoints(
        &self,
        principal: &str,
        connection_id: &str,
    ) -> Result<Vec<Endpoint>, InboundError> {
        let allowed = self.allowed(principal, connection_id, Capability::EndpointRead)?;
        Ok(allowed.adapter.endpoints(allowed.account()).await?)
    }

    /// Received mail, without envelope images or contents. Needs
    /// `mailbox.read.items`.
    pub async fn inbox(
        &self,
        principal: &str,
        request: InboxRequest,
    ) -> Result<InboxPage, InboundError> {
        if let Some(endpoint_id) = &request.endpoint_id {
            check_id("endpoint", endpoint_id)?;
        }
        if request
            .cursor
            .as_ref()
            .is_some_and(|c| c.is_empty() || c.len() > MAX_CURSOR_LEN)
        {
            return invalid("the cursor is not one this server returned");
        }
        let received_after = match &request.received_after {
            Some(text) => match clock::parse_rfc3339(text) {
                Ok(time) => Some(clock::to_rfc3339(time)),
                Err(_) => {
                    return invalid(
                        "received_after must be an RFC 3339 time, like 2026-01-31T00:00:00Z",
                    );
                }
            },
            None => None,
        };
        let limit = request.limit.unwrap_or(DEFAULT_PAGE_SIZE);
        if !(1..=MAX_PAGE_SIZE).contains(&limit) {
            return invalid(format!("limit must be from 1 to {MAX_PAGE_SIZE}"));
        }
        let query = InboxQuery {
            endpoint_id: request.endpoint_id,
            received_after,
            limit,
            cursor: request.cursor,
        };

        let allowed = self.allowed(
            principal,
            &request.connection_id,
            Capability::MailboxReadItems,
        )?;
        let mut page = allowed.adapter.inbox(allowed.account(), &query).await?;
        // A listing says what arrived. Images and contents are read one
        // item at a time, where their own permissions are checked.
        for item in &mut page.items {
            item.envelope_image_url = None;
            item.scan = None;
        }
        Ok(page)
    }

    /// One received item, with the envelope image and scanned contents
    /// only where this client may read them. Needs `mailbox.read.items`.
    pub async fn read(
        &self,
        principal: &str,
        connection_id: &str,
        item_id: &str,
    ) -> Result<ReadItem, InboundError> {
        check_id("item", item_id)?;
        let allowed = self.allowed(principal, connection_id, Capability::MailboxReadItems)?;
        let Some(mut item) = allowed
            .adapter
            .inbound_item(allowed.account(), item_id)
            .await?
        else {
            return invalid(format!("no item {item_id:?}; call correspondence_inbox"));
        };

        let mut withheld = Vec::new();
        if item.envelope_image_url.is_some()
            && !self
                .evaluate(principal, connection_id, Capability::MailboxReadEnvelopes)?
                .is_allowed()
        {
            item.envelope_image_url = None;
            withheld.push(Capability::MailboxReadEnvelopes);
        }
        if item.scan.is_some()
            && !self
                .evaluate(principal, connection_id, Capability::MailboxReadScans)?
                .is_allowed()
        {
            item.scan = None;
            withheld.push(Capability::MailboxReadScans);
        }
        Ok(ReadItem { item, withheld })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_that_could_change_a_provider_url_are_refused() {
        for id in ["mi_123", "3f9a-1c0b", "A1"] {
            assert!(check_id("item", id).is_ok(), "{id}");
        }
        for id in ["", "../locations", "a/b", "a?b=c", "a b", "a%2Fb", "é"] {
            assert!(check_id("item", id).is_err(), "{id}");
        }
        assert!(check_id("item", &"a".repeat(MAX_ID_LEN + 1)).is_err());
    }
}
