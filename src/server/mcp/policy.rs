//! Central authorization rules shared by every MCP device operation.

use crate::server::machine_tokens::{MachinePrincipal, MachineScope};

use super::error::McpError;

/// Requires both a capability and an explicit stable device grant.
pub(super) fn require_device(
    principal: &MachinePrincipal,
    scope: MachineScope,
    device_id: &str,
    request_id: &serde_json::Value,
) -> Result<(), McpError> {
    if !principal.grant.scopes.contains(&scope) {
        return Err(McpError::forbidden(
            format!("Missing required scope {scope}"),
            Some(request_id.clone()),
        ));
    }
    if !principal.grant.device_ids.contains(device_id) {
        return Err(McpError::forbidden(
            "Device is not included in this grant",
            Some(request_id.clone()),
        ));
    }
    Ok(())
}

/// Operation access follows the stable grant rather than the replaceable credential.
pub(super) fn require_owner(
    principal: &MachinePrincipal,
    owner_grant: uuid::Uuid,
    request_id: &serde_json::Value,
) -> Result<(), McpError> {
    if principal.grant.id != owner_grant {
        return Err(McpError::forbidden(
            "Operation belongs to another grant",
            Some(request_id.clone()),
        ));
    }
    Ok(())
}
