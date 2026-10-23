//! MCP server capability discovery responses.

use serde_json::Value;

use super::schema::{PROTOCOL_VERSION, server_metadata};

/// Advertises only tools because subscriptions and server-to-client requests are absent.
pub(super) fn result() -> Value {
    serde_json::json!({
        "resultType": "complete",
        "supportedVersions": [PROTOCOL_VERSION],
        "capabilities": { "tools": {} },
        "_meta": server_metadata(),
        "instructions": "Redoor operates on the devices this access token may use: list devices, browse, read, write, search, copy, move and delete files, run commands, and start or stop managed agents. tools/list only shows the tools the token's permissions allow. Small files are read and written inline; use the redoor remote CLI for large local transfers.",
        "ttlMs": 300000,
        "cacheScope": "private"
    })
}
