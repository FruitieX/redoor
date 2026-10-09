use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::error::McpError;

/// Published stateless MCP revision implemented by this server.
pub(crate) const PROTOCOL_VERSION: &str = "2026-07-28";

/// A decoded JSON-RPC request keeps method parameters schema-neutral for future tool modules.
#[derive(Debug, Deserialize)]
pub(super) struct JsonRpcRequest {
    pub(super) jsonrpc: String,
    pub(super) id: Value,
    pub(super) method: String,
    pub(super) params: Value,
}

/// Required stateless metadata is validated structurally on every independent request.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestMeta {
    #[serde(rename = "io.modelcontextprotocol/protocolVersion")]
    protocol_version: String,
    #[serde(rename = "io.modelcontextprotocol/clientCapabilities")]
    client_capabilities: Map<String, Value>,
    #[serde(rename = "io.modelcontextprotocol/clientInfo", default)]
    client_info: Option<ClientInfo>,
}

/// Optional identity is diagnostic only and therefore never enters authorization policy.
#[derive(Debug, Deserialize)]
struct ClientInfo {
    name: String,
    version: String,
}

/// Successful JSON-RPC responses mirror the request ID exactly.
#[derive(Debug, Serialize)]
pub(super) struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    result: Value,
}

impl JsonRpcResponse {
    /// Wraps a schema-specific result without introducing transport state.
    pub(super) fn complete(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result,
        }
    }
}

impl JsonRpcRequest {
    /// Rejects JSON-RPC responses, notifications, null IDs, and missing required metadata.
    pub(super) fn validate(value: Value) -> Result<(Self, String), McpError> {
        let id = value
            .get("id")
            .cloned()
            .filter(|id| id.is_string() || id.is_i64() || id.is_u64());
        let request: Self = serde_json::from_value(value)
            .map_err(|_| McpError::invalid_request("Invalid JSON-RPC request", id.clone()))?;
        if request.jsonrpc != "2.0"
            || request.id.is_null()
            || !(request.id.is_string() || request.id.is_i64() || request.id.is_u64())
            || request.method.is_empty()
            || !request.params.is_object()
        {
            return Err(McpError::invalid_request("Invalid JSON-RPC request", id));
        }
        let meta_value = request.params.get("_meta").cloned().ok_or_else(|| {
            McpError::invalid_params("params._meta is required", Some(request.id.clone()))
        })?;
        let meta: RequestMeta = serde_json::from_value(meta_value).map_err(|_| {
            McpError::invalid_params(
                "Protocol version and client capabilities are required in params._meta",
                Some(request.id.clone()),
            )
        })?;
        // Reading optional identity fields validates their required shape without trusting their values.
        if let Some(info) = meta.client_info
            && (info.name.is_empty() || info.version.is_empty())
        {
            return Err(McpError::invalid_params(
                "Client identity name and version must be non-empty",
                Some(request.id.clone()),
            ));
        }
        let _ = meta.client_capabilities;
        Ok((request, meta.protocol_version))
    }
}

/// Reused on every result so direct first-request tool calls still identify the server.
pub(super) fn server_metadata() -> Value {
    serde_json::json!({
        "io.modelcontextprotocol/serverInfo": {
            "name": "redoor",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}
