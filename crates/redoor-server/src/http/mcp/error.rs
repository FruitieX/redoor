//! JSON-RPC error envelopes paired with the HTTP status each MCP failure requires.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;

/// JSON-RPC error envelope preserves a readable request ID whenever parsing reached it.
#[derive(Debug, Serialize)]
pub(super) struct ErrorEnvelope {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    error: ErrorBody,
}

/// Error bodies carry standard or MCP-reserved codes with optional negotiation data.
#[derive(Debug, Serialize)]
struct ErrorBody {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

/// Associates the protocol error with its required HTTP transport status.
#[derive(Debug)]
pub(super) struct McpError {
    pub(super) status: StatusCode,
    pub(super) code: i32,
    pub(super) message: String,
    pub(super) data: Option<Value>,
    pub(super) id: Option<Value>,
}

impl McpError {
    /// Reports JSON syntax failures without claiming a request ID was successfully decoded.
    pub(super) fn parse(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, -32700, message, None)
    }

    /// Reports an invalid JSON-RPC envelope or required per-request metadata.
    pub(super) fn invalid_request(message: impl Into<String>, id: Option<Value>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, -32600, message, id)
    }

    /// Reports method or tool parameters that fail their published schema.
    pub(super) fn invalid_params(message: impl Into<String>, id: Option<Value>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, -32602, message, id)
    }

    /// Authorization failures use HTTP 403 without disguising a known device as protocol syntax.
    pub(super) fn forbidden(message: impl Into<String>, id: Option<Value>) -> Self {
        Self::new(StatusCode::FORBIDDEN, -32003, message, id)
    }

    /// Converts an access decision from `Principal::require` into the MCP forbidden error, so
    /// REST and MCP report the same reason.
    pub(super) fn denied(error: crate::http::error::ApiError, id: &Value) -> Self {
        Self::forbidden(error.to_string(), Some(id.clone()))
    }

    /// Remote failures remain tool errors while preserving a stable JSON-RPC envelope.
    pub(super) fn operation(message: impl Into<String>, id: Option<Value>) -> Self {
        Self::new(StatusCode::CONFLICT, -32010, message, id)
    }

    /// Missing retained resources use HTTP 404 while preserving the operation error code.
    pub(super) fn operation_not_found(message: impl Into<String>, id: Option<Value>) -> Self {
        Self::new(StatusCode::NOT_FOUND, -32010, message, id)
    }

    /// Modern HTTP uses 404 plus JSON-RPC method-not-found to distinguish legacy endpoints.
    pub(super) fn method_not_found(method: &str, id: Option<Value>) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            -32601,
            format!("Method not found: {method}"),
            id,
        )
    }

    /// Header/body disagreement uses the protocol-reserved HeaderMismatch code.
    pub(super) fn header_mismatch(message: impl Into<String>, id: Option<Value>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, -32020, message, id)
    }

    /// Unsupported versions identify the only revision this intentionally narrow server speaks.
    pub(super) fn unsupported_version(requested: &str, supported: &str, id: Option<Value>) -> Self {
        let mut error = Self::new(
            StatusCode::BAD_REQUEST,
            -32022,
            "Unsupported protocol version",
            id,
        );
        error.data = Some(serde_json::json!({ "supported": [supported], "requested": requested }));
        error
    }

    /// Keeps response construction uniform across validation layers.
    fn new(status: StatusCode, code: i32, message: impl Into<String>, id: Option<Value>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            data: None,
            id,
        }
    }
}

impl IntoResponse for McpError {
    /// Serializes protocol failures as JSON even when the associated HTTP status is non-200.
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                jsonrpc: "2.0",
                id: self.id,
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                    data: self.data,
                },
            }),
        )
            .into_response()
    }
}
