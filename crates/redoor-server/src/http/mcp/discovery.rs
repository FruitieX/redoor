//! MCP server capability discovery responses.

use serde_json::Value;

use super::schema::{PROTOCOL_VERSION, server_metadata};

/// Advertises only stable tools because subscriptions and server-to-client requests are absent.
pub(super) fn result() -> Value {
    serde_json::json!({
        "resultType": "complete",
        "supportedVersions": [PROTOCOL_VERSION],
        "capabilities": { "tools": {} },
        "_meta": server_metadata(),
        "instructions": "Redoor exposes an authenticated hello smoke-test tool.",
        "ttlMs": 300000,
        "cacheScope": "private"
    })
}
