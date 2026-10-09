use axum::http::HeaderMap;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;

use super::{
    error::McpError,
    schema::{JsonRpcRequest, PROTOCOL_VERSION},
};

/// Validates all mirrored headers before dispatch so intermediaries and the body share one truth.
pub(super) fn validate_headers(
    headers: &HeaderMap,
    request: &JsonRpcRequest,
    body_version: &str,
) -> Result<(), McpError> {
    let id = Some(request.id.clone());
    let header_version = required_header(headers, "mcp-protocol-version", id.clone())?;
    if header_version != body_version {
        return Err(McpError::header_mismatch(
            "MCP-Protocol-Version header does not match params._meta",
            id,
        ));
    }
    if body_version != PROTOCOL_VERSION {
        return Err(McpError::unsupported_version(
            body_version,
            PROTOCOL_VERSION,
            Some(request.id.clone()),
        ));
    }
    let method = required_header(headers, "mcp-method", Some(request.id.clone()))?;
    if method != request.method {
        return Err(McpError::header_mismatch(
            "Mcp-Method header does not match the request method",
            Some(request.id.clone()),
        ));
    }
    if request.method == "tools/call" {
        let expected = request
            .params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                McpError::invalid_params(
                    "tools/call requires a string name",
                    Some(request.id.clone()),
                )
            })?;
        let encoded = required_header(headers, "mcp-name", Some(request.id.clone()))?;
        let decoded = decode_header_value(&encoded)
            .map_err(|message| McpError::header_mismatch(message, Some(request.id.clone())))?;
        if decoded != expected {
            return Err(McpError::header_mismatch(
                "Mcp-Name header does not match params.name",
                Some(request.id.clone()),
            ));
        }
    }
    Ok(())
}

/// Missing or non-text standard headers are all HeaderMismatch failures in this revision.
fn required_header(headers: &HeaderMap, name: &str, id: Option<Value>) -> Result<String, McpError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .ok_or_else(|| {
            McpError::header_mismatch(
                format!("Required {name} header is missing or malformed"),
                id,
            )
        })
}

/// Decodes the exact case-sensitive Base64 sentinel and rejects ambiguous malformed sentinels.
fn decode_header_value(value: &str) -> Result<String, String> {
    if let Some(encoded) = value
        .strip_prefix("=?base64?")
        .and_then(|rest| rest.strip_suffix("?="))
    {
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| "Mcp-Name contains invalid Base64".to_string())?;
        return String::from_utf8(bytes).map_err(|_| "Mcp-Name Base64 is not UTF-8".to_string());
    }
    if value.starts_with("=?base64?") || value.ends_with("?=") || value.trim() != value {
        return Err("Mcp-Name uses invalid header encoding".to_string());
    }
    Ok(value.to_string())
}
