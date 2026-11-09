//! The MCP execution plane (design.md §9, §20.3). All rmcp-specific code
//! stays in this module so SDK upgrades are local.

use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{Json, ServerHandler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};

use crate::connections::{Connection, ConnectionStatus};
use crate::core::{Capability, clock};
use crate::delegation::{self, Constraints, Decision, OperationContext, Reason};
use crate::providers::{Environment, ProviderInfo, Registry};
use crate::store::{Store, StoreError};

#[derive(Clone)]
pub struct PostalServer {
    providers: Registry,
    store: Store,
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

fn internal(e: StoreError) -> String {
    tracing::error!(error = %e, "storage error");
    "internal storage error".into()
}

#[tool_router]
impl PostalServer {
    pub fn new(providers: Registry, store: Store, principal: impl Into<String>) -> Self {
        Self {
            providers,
            store,
            principal: principal.into(),
            tool_router: Self::tool_router(),
        }
    }

    /// Connections named by at least one active grant to this client. A
    /// client with no grant on a connection cannot learn that it exists.
    fn visible_connections(&self) -> Result<Vec<Connection>, String> {
        let now = clock::now();
        let grants = self.store.grants(Some(&self.principal)).map_err(internal)?;
        let visible: Vec<&String> = grants
            .iter()
            .filter(|g| g.is_active(now))
            .flat_map(|g| &g.connection_ids)
            .collect();
        let mut connections = self.store.connections().map_err(internal)?;
        connections.retain(|c| visible.contains(&&c.id));
        Ok(connections)
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
        Ok(Json(PermissionResult {
            allowed: evaluation.is_allowed(),
            decision: evaluation.decision,
            reason: evaluation.reason,
            message: evaluation.message,
            grant_id: evaluation.grant_id,
            policy_version: evaluation.policy_version,
            approval_available: false,
        }))
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
