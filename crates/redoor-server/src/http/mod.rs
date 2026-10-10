//! Axum HTTP surface: REST handlers, websocket endpoints, MCP, and UI embedding.

pub(crate) mod agent_client;
mod agent_configuration;
mod agent_helpers;
mod agent_logs;
mod agent_ws;
mod agents;
mod archive_upload;
pub(crate) mod auth;
mod copy;
mod diffs;
pub(crate) mod error;
mod exec;
#[cfg(test)]
pub(crate) mod fake_agent;
mod file_edit;
mod files;
mod git;
mod logging_levels;
mod logs;
pub mod machine_tokens;
pub(crate) mod mcp;
mod mcp_tokens;
mod moves;
#[cfg(target_os = "linux")]
mod pam;
mod protocol_mismatches;
mod raw;
mod restart;
mod retry_agent_start;
pub(crate) mod routes;
mod server_info;
pub(crate) mod services;
pub mod state;
mod terminals;
mod transfer_cancellation;
mod transfer_progress;
mod trash;
mod ui;
pub(crate) mod upload_requests;
mod user_state;
pub(crate) mod watchdog;
mod websocket_security;
mod ws;
