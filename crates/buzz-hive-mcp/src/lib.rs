//! A stdio MCP server whose tools are the rows of `buzz_hive_api::tools::TOOLS`.
//!
//! The tools are listed and called from that table at run time, so the shim has no tool of
//! its own: a row added to the table is a tool here with no change to this crate.

mod client;

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::stdio;
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use serde_json::Value;

pub use client::{CallError, Clock, Config, HiveClient, SystemClock, RENEW_BEFORE_SECS};

const INSTRUCTIONS: &str = "Tools for Beehive, the workflow board. A stage task in chat names the \
card, the stage, the visit and the results it accepts. When you finish the stage, call stage_done \
with that card_id, stage and visit and one of those results.";

/// The MCP tool for each row of the table.
pub fn tool_list() -> Vec<Tool> {
    buzz_hive_api::tools::TOOLS
        .iter()
        .map(|row| {
            let schema = match (row.schema)() {
                Value::Object(map) => map,
                _ => JsonObject::new(),
            };
            Tool::new(row.name, row.description, Arc::new(schema))
        })
        .collect()
}

#[derive(Clone)]
pub struct Hive {
    client: Arc<HiveClient>,
}

impl Hive {
    pub fn new(client: Arc<HiveClient>) -> Self {
        Self { client }
    }

    /// Forwards one tool call. A refusal comes back as a tool error that carries the Connect
    /// code, so the agent can read why and try something else.
    pub async fn forward(
        &self,
        name: &str,
        arguments: Option<JsonObject>,
    ) -> Option<CallToolResult> {
        let row = buzz_hive_api::tools::TOOLS
            .iter()
            .find(|row| row.name == name)?;
        let body = Value::Object(arguments.unwrap_or_default());
        Some(match self.client.call(row.route, body).await {
            Ok(reply) => CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string_pretty(&reply).unwrap_or_default(),
            )]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
        })
    }
}

impl ServerHandler for Hive {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "buzz-hive-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tool_list()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match self.forward(&request.name, request.arguments).await {
            Some(result) => Ok(result.into()),
            None => Err(ErrorData::invalid_params(
                format!("there is no tool named {}", request.name),
                None,
            )),
        }
    }
}

/// Serves the tools on stdin and stdout until the client closes them.
pub async fn serve_stdio(
    config: Config,
    clock: Arc<dyn Clock>,
) -> Result<(), Box<dyn std::error::Error>> {
    let hive = Hive::new(Arc::new(HiveClient::new(config, clock)));
    let service = hive.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
