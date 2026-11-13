//! The MCP execution plane (design.md §9, §20.3). All rmcp-specific code
//! stays in this module so SDK upgrades are local.

use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{Json, ServerHandler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};

use std::sync::Arc;

use crate::connections::{Connection, ConnectionStatus, SecretStore};
use crate::core::{Capability, Money, clock};
use crate::delegation::{self, Constraints, Decision, Evaluation, OperationContext, Reason};
use std::path::Path;

use crate::documents::{self, DocumentDirs, DocumentFormat};
use crate::operations::{
    Address, Operation, OperationError, OperationStatus, Operations, PriceKind, QuoteRequest,
    SendOutcome,
};
use crate::providers::{Environment, ProviderInfo, Registry, Tracking};
use crate::store::{Store, StoreError};

#[derive(Clone)]
pub struct PostalServer {
    providers: Registry,
    store: Store,
    operations: Operations,
    document_dirs: DocumentDirs,
    /// The client this instance serves. It comes from server configuration
    /// (`serve --client`), never from tool arguments (design.md §5.2).
    principal: String,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ProvidersList {
    pub providers: Vec<ProviderInfo>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProviderRef {
    /// Provider id, as returned by `providers_list` (e.g. "mock", "lob").
    pub provider: String,
}

/// A connected account, as shown to the client. Never includes credentials.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ConnectionView {
    pub connection_id: String,
    pub provider: String,
    pub environment: Environment,
    pub status: ConnectionStatus,
    /// What the account can do. What this client may do with it is a
    /// separate question: see `permissions_get`.
    pub capabilities: Vec<Capability>,
}

impl From<Connection> for ConnectionView {
    fn from(c: Connection) -> Self {
        Self {
            connection_id: c.id,
            provider: c.provider,
            environment: c.environment,
            status: c.status,
            capabilities: c.capabilities,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ConnectionsList {
    pub connections: Vec<ConnectionView>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConnectionRef {
    /// Connection id, as returned by `connections_list`.
    pub connection_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GrantView {
    pub grant_id: String,
    pub connection_ids: Vec<String>,
    pub actions: Vec<Capability>,
    pub constraints: Constraints,
    /// RFC 3339; absent if the grant lasts until revoked.
    pub expires_at: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Permissions {
    /// The client name this server instance serves.
    pub client: String,
    /// Active grants only. Empty means nothing is permitted.
    pub grants: Vec<GrantView>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PermissionCheck {
    pub connection_id: String,
    /// The capability the operation needs, e.g. "postal.send.letter".
    pub action: Capability,
    /// Details of the proposed operation. Limits in the grant are checked
    /// against these; a limit whose detail is missing denies.
    #[serde(default)]
    pub context: OperationContext,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PermissionResult {
    pub allowed: bool,
    pub decision: Decision,
    pub reason: Option<Reason>,
    pub message: Option<String>,
    pub grant_id: Option<String>,
    pub policy_version: Option<u32>,
    /// Whether an approval workflow can be started from here. Not yet: the
    /// user changes permissions with `postal-mcp admin`.
    pub approval_available: bool,
}

impl From<Evaluation> for PermissionResult {
    fn from(evaluation: Evaluation) -> Self {
        Self {
            allowed: evaluation.is_allowed(),
            decision: evaluation.decision,
            reason: evaluation.reason,
            message: evaluation.message,
            grant_id: evaluation.grant_id,
            policy_version: evaluation.policy_version,
            approval_available: false,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DocumentPrepare {
    /// The document as inline text or Markdown. Give this or `path`.
    pub content: Option<String>,
    /// The format of `content`: "text" (the default) or "markdown".
    #[serde(default)]
    pub format: DocumentFormat,
    /// Absolute path of a PDF file to import. It must be inside a
    /// directory the user allowed when starting the server.
    pub path: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DocumentView {
    pub document_id: String,
    /// SHA-256 of the content as imported.
    pub sha256: String,
    pub size_bytes: usize,
    pub page_count: u32,
    pub format: DocumentFormat,
    /// True when the page count is an estimate from a layout rule. A PDF's
    /// count is exact.
    pub page_count_estimated: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct QuoteParams {
    pub connection_id: String,
    /// The product to send. Supported: "letter".
    pub product: String,
    /// The recipient.
    pub to: Address,
    /// The return address, where the provider needs one.
    pub from: Option<Address>,
    /// Documents from `documents_prepare`, in printing order.
    pub document_ids: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct QuoteView {
    pub quote_id: String,
    pub price_kind: PriceKind,
    /// Absent when the provider cannot price the item.
    pub price: Option<Money>,
    pub page_count: u32,
    /// RFC 3339. After this, request a new quote.
    pub expires_at: String,
    /// Whether sending this would be permitted right now. It is decided
    /// again when the item is actually sent.
    pub authorization: PermissionResult,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendParams {
    /// From `correspondence_quote`. A quote can be sent once; sending it
    /// again returns the same operation and sends nothing new.
    pub quote_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OperationRef {
    /// From `correspondence_send`.
    pub operation_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct OperationView {
    pub operation_id: String,
    pub status: OperationStatus,
    /// What the status means and what to do about it.
    pub note: String,
    pub connection_id: String,
    pub action: Capability,
    /// The provider's own id for the order, once accepted.
    pub provider_reference: Option<String>,
    pub error: Option<String>,
    /// The provider's delivery report. Present only for an accepted item,
    /// and only if this client may read status on the connection.
    pub delivery: Option<Tracking>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<Operation> for OperationView {
    fn from(o: Operation) -> Self {
        let note = match o.status {
            OperationStatus::Pending => "Being submitted. Check correspondence_status.",
            OperationStatus::Accepted => {
                "Accepted by the provider. Acceptance is not proof of mailing or delivery."
            }
            OperationStatus::Failed => "The provider did not accept this. Nothing was charged.",
            OperationStatus::Uncertain => {
                "The provider may or may not have accepted this. Do not send it again: \
                 that could mail it twice. Check correspondence_status later; if it stays \
                 uncertain, the user must resolve it with `postal-mcp admin resolve`."
            }
        };
        Self {
            operation_id: o.id,
            status: o.status,
            note: note.into(),
            connection_id: o.connection_id,
            action: o.action,
            provider_reference: o.provider_reference,
            error: o.error,
            delivery: None,
            created_at: clock::to_rfc3339(o.created_at),
            updated_at: clock::to_rfc3339(o.updated_at),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SendResult {
    /// True only if the provider accepted the item.
    pub accepted: bool,
    /// Present unless the send was denied.
    pub operation: Option<OperationView>,
    /// Present when the send was denied: why.
    pub authorization: Option<PermissionResult>,
}

fn operation_error(e: OperationError) -> String {
    match e {
        OperationError::Store(e) => internal(e),
        other => other.to_string(),
    }
}

fn internal(e: StoreError) -> String {
    tracing::error!(error = %e, "storage error");
    "internal storage error".into()
}

#[tool_router]
impl PostalServer {
    pub fn new(
        providers: Registry,
        store: Store,
        secrets: Arc<dyn SecretStore>,
        document_dirs: DocumentDirs,
        principal: impl Into<String>,
    ) -> Self {
        Self {
            document_dirs,
            operations: Operations::new(store.clone(), providers.clone(), secrets),
            providers,
            store,
            principal: principal.into(),
            tool_router: Self::tool_router(),
        }
    }

    /// Available before any account is connected (design.md §9.1).
    #[tool(
        description = "List the correspondence providers this server supports, with the channels and capabilities each can offer. Connecting an account and delegating permissions are separate steps.",
        annotations(read_only_hint = true)
    )]
    async fn providers_list(&self) -> Json<ProvidersList> {
        Json(ProvidersList {
            providers: self.providers.list(),
        })
    }

    #[tool(
        description = "Describe one provider: its channels, the capabilities it can offer, how accounts authenticate, and which environments exist. A connected account may support fewer capabilities.",
        annotations(read_only_hint = true)
    )]
    async fn providers_capabilities(
        &self,
        Parameters(ProviderRef { provider }): Parameters<ProviderRef>,
    ) -> Result<Json<ProviderInfo>, String> {
        self.providers
            .get(&provider)
            .map(|a| Json(a.info()))
            .ok_or_else(|| format!("unknown provider {provider:?}; call providers_list"))
    }

    fn visible_connections(&self) -> Result<Vec<Connection>, String> {
        delegation::visible_connections(&self.store, &self.principal, clock::now())
            .map_err(internal)
    }

    #[tool(
        description = "List the connected provider accounts this client has been granted access to. Accounts are connected by the user with `postal-mcp admin connect`, never through this server.",
        annotations(read_only_hint = true)
    )]
    async fn connections_list(&self) -> Result<Json<ConnectionsList>, String> {
        Ok(Json(ConnectionsList {
            connections: self
                .visible_connections()?
                .into_iter()
                .map(Into::into)
                .collect(),
        }))
    }

    #[tool(
        description = "Show the status and account capabilities of one connection this client has access to.",
        annotations(read_only_hint = true)
    )]
    async fn connections_status(
        &self,
        Parameters(ConnectionRef { connection_id }): Parameters<ConnectionRef>,
    ) -> Result<Json<ConnectionView>, String> {
        self.visible_connections()?
            .into_iter()
            .find(|c| c.id == connection_id)
            .map(|c| Json(c.into()))
            .ok_or_else(|| {
                format!("no accessible connection {connection_id:?}; call connections_list")
            })
    }

    #[tool(
        description = "Return the permissions the user has delegated to this client: which actions on which connections, and under what limits. Permissions are granted and revoked only by the user with `postal-mcp admin grant` / `revoke`; this server cannot change them.",
        annotations(read_only_hint = true)
    )]
    async fn permissions_get(&self) -> Result<Json<Permissions>, String> {
        let now = clock::now();
        let grants = self.store.grants(Some(&self.principal)).map_err(internal)?;
        Ok(Json(Permissions {
            client: self.principal.clone(),
            grants: grants
                .into_iter()
                .filter(|g| g.is_active(now))
                .map(|g| GrantView {
                    grant_id: g.id,
                    connection_ids: g.connection_ids,
                    actions: g.actions,
                    constraints: g.constraints,
                    expires_at: g.expires_at.map(clock::to_rfc3339),
                })
                .collect(),
        }))
    }

    #[tool(
        description = "Check whether a proposed operation is permitted right now, without performing it. Returns allow, deny (with a reason) or approval_required. If denied for lack of permission, tell the user what to grant; do not try to work around it.",
        annotations(read_only_hint = true)
    )]
    async fn permissions_check(
        &self,
        Parameters(check): Parameters<PermissionCheck>,
    ) -> Result<Json<PermissionResult>, String> {
        let evaluation = delegation::check(
            &self.store,
            &self.principal,
            &check.connection_id,
            check.action,
            &check.context,
            clock::now(),
        )
        .map_err(internal)?;
        Ok(Json(evaluation.into()))
    }

    #[tool(
        description = "Import a document to send. Give plain text or Markdown inline as `content`, or the `path` of a PDF file (US Letter pages) in a directory the user has allowed. Returns a document_id for correspondence_quote, with its hash and page count. The document is visible only to this client.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn documents_prepare(
        &self,
        Parameters(request): Parameters<DocumentPrepare>,
    ) -> Result<Json<DocumentView>, String> {
        let document = match (&request.content, &request.path) {
            (Some(content), None) => documents::prepare(
                &self.store,
                &self.principal,
                request.title,
                request.format,
                content,
            ),
            (None, Some(path)) => documents::import_pdf(
                &self.store,
                &self.principal,
                request.title,
                Path::new(path),
                &self.document_dirs,
            ),
            _ => return Err("give exactly one of `content` or `path`".into()),
        }
        .map_err(|e| match e {
            documents::DocumentError::Store(e) => internal(e),
            other => other.to_string(),
        })?;
        Ok(Json(DocumentView {
            document_id: document.id,
            format: document.format,
            sha256: document.sha256,
            size_bytes: document.content.len(),
            page_count: document.page_count,
            page_count_estimated: document.page_count_estimated,
        }))
    }

    #[tool(
        description = "Price a piece of mail and check whether this client may send it. Sends nothing and costs nothing. Returns a quote_id for correspondence_send.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn correspondence_quote(
        &self,
        Parameters(request): Parameters<QuoteParams>,
    ) -> Result<Json<QuoteView>, String> {
        let (quote, evaluation) = self
            .operations
            .quote(
                &self.principal,
                QuoteRequest {
                    connection_id: request.connection_id,
                    product: request.product,
                    to: request.to,
                    from: request.from,
                    document_ids: request.document_ids,
                },
            )
            .await
            .map_err(operation_error)?;
        Ok(Json(QuoteView {
            quote_id: quote.id,
            price_kind: quote.price.kind,
            price: quote.price.amount,
            page_count: quote.item.page_count,
            expires_at: clock::to_rfc3339(quote.expires_at),
            authorization: evaluation.into(),
        }))
    }

    #[tool(
        description = "Send the quoted item through the provider. This mails a real letter and spends real money on a live account, and cannot be undone. It succeeds only within the permissions the user has delegated; a denial explains why and must not be worked around. Sending the same quote again is safe: it returns the same operation.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn correspondence_send(
        &self,
        Parameters(SendParams { quote_id }): Parameters<SendParams>,
    ) -> Result<Json<SendResult>, String> {
        let outcome = self
            .operations
            .send(&self.principal, &quote_id)
            .await
            .map_err(operation_error)?;
        Ok(Json(match outcome {
            SendOutcome::Operation(operation) => SendResult {
                accepted: operation.status == OperationStatus::Accepted,
                operation: Some(operation.into()),
                authorization: None,
            },
            SendOutcome::Denied(evaluation) => SendResult {
                accepted: false,
                operation: None,
                authorization: Some(evaluation.into()),
            },
        }))
    }

    #[tool(
        description = "Report where a sent item stands: pending, accepted, failed or uncertain. An uncertain item is checked with the provider first. For an accepted item it adds the provider's delivery report, if this client may read status.",
        annotations(read_only_hint = true)
    )]
    async fn correspondence_status(
        &self,
        Parameters(OperationRef { operation_id }): Parameters<OperationRef>,
    ) -> Result<Json<OperationView>, String> {
        let operation = self
            .operations
            .status(&self.principal, &operation_id)
            .await
            .map_err(operation_error)?;
        let delivery = self
            .operations
            .track(&self.principal, &operation)
            .await
            .map_err(operation_error)?;
        Ok(Json(OperationView {
            delivery,
            ..operation.into()
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PostalServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Universal Correspondence MCP: send and receive postal mail and faxes through \
                 the user's connected provider accounts, within the permissions the user has \
                 delegated. The server enforces those permissions; it cannot grant them.",
            )
    }
}
