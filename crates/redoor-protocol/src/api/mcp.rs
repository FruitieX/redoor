//! MCP machine-credential management bodies.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Browser-facing representation of one supported MCP authorization scope.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum McpTokenScope {
    #[serde(rename = "devices:read")]
    DevicesRead,
    #[serde(rename = "files:read")]
    FilesRead,
    #[serde(rename = "files:write")]
    FilesWrite,
    #[serde(rename = "files:delete")]
    FilesDelete,
    #[serde(rename = "commands:execute")]
    CommandsExecute,
    #[serde(rename = "agents:manage")]
    AgentsManage,
}

/// Validated browser input used to create a non-recoverable MCP bearer credential.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateMcpTokenRequest {
    pub name: String,
    pub scopes: Vec<McpTokenScope>,
    pub device_ids: Vec<String>,
    /// Optional RFC 3339 timestamp; null means no automatic expiry.
    pub expires_at: Option<String>,
}

/// Replaces editable policy without changing the bearer secret or expiry.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateMcpTokenRequest {
    pub name: String,
    pub scopes: Vec<McpTokenScope>,
    pub device_ids: Vec<String>,
}

/// Returns saved policy without revealing or replacing credential material.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateMcpTokenResponse {
    pub credential: McpTokenMetadataResponse,
}

/// Secret-free lifecycle and stable-grant metadata safe for settings-page caching.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpTokenMetadataResponse {
    pub credential_id: String,
    pub grant_id: String,
    pub name: String,
    pub scopes: Vec<McpTokenScope>,
    pub device_ids: Vec<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub revoked_at: Option<String>,
    pub last_used_at: Option<String>,
}

/// List envelope permits future paging without exposing verifiers.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpTokenListResponse {
    pub tokens: Vec<McpTokenMetadataResponse>,
}

/// One-time creation or rotation result deliberately separates its secret.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpTokenSecretResponse {
    pub token: String,
    pub credential: McpTokenMetadataResponse,
}

/// Confirms revocation without echoing any credential material.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RevokeMcpTokenResponse {
    pub revoked: bool,
}
