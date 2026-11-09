//! The MCP execution plane (design.md §9, §20.3). All rmcp-specific code
//! stays in this module so SDK upgrades are local.

use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{Json, ServerHandler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};

use crate::providers::{ProviderInfo, Registry};

#[derive(Clone)]
pub struct PostalServer {
    providers: Registry,
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

#[tool_router]
impl PostalServer {
    pub fn new(providers: Registry) -> Self {
        Self {
            providers,
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
