use serde_json::Value;

use super::{discovery, error::McpError, schema::JsonRpcRequest, tools};
use crate::http::{machine_tokens::MachinePrincipal, state::ServerState};

/// Routes core methods while leaving tool catalog/implementation in its own module.
pub(super) async fn request(
    state: &ServerState,
    principal: &MachinePrincipal,
    request: &JsonRpcRequest,
) -> Result<Value, McpError> {
    match request.method.as_str() {
        "server/discover" => Ok(discovery::result()),
        "tools/list" => tools::list_result(&request.params, &request.id),
        "tools/call" => tools::call_result(state, principal, &request.params, &request.id).await,
        method => Err(McpError::method_not_found(method, Some(request.id.clone()))),
    }
}
