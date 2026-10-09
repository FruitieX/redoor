//! Filename and content search bodies shared by REST and the agent protocol.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Bounds per-match context so the fixed result ceiling also bounds response memory.
pub const MAX_GREP_CONTEXT_LINES: u64 = 20;

/// Controls how letter case affects path and content search matching.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[ts(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CaseSensitivity {
    /// Uses case-insensitive matching until the query contains an uppercase letter.
    #[default]
    Smart,
    /// Requires query and candidate letter case to match exactly.
    Sensitive,
    /// Ignores letter case regardless of the query's capitalization.
    Insensitive,
}

/// Preserves the public REST default when a newer agent receives a command from an older server.
pub(crate) fn default_file_search_timeout_seconds() -> u64 {
    5
}

/// Preserves default-enabled command options when older senders omit them.
pub(crate) fn default_true() -> bool {
    true
}

/// Describes one matching remote path without requiring additional metadata calls.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileSearchEntry {
    pub name: String,
    pub path: String,
    #[serde(rename = "type")]
    pub file_type: String,
}

/// Carries bounded filename-search controls while the route identifies the agent and root path.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FindRequest {
    pub query: String,
    #[serde(default = "default_file_search_timeout_seconds")]
    #[ts(type = "number")]
    pub timeout: u64,
    #[serde(default)]
    pub include_hidden: bool,
    #[serde(default = "default_true")]
    pub respect_gitignore: bool,
    #[serde(default)]
    pub case_sensitivity: CaseSensitivity,
}

/// Returns the best paths discovered before traversal completed or reached its deadline.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileSearchResponse {
    pub results: Vec<FileSearchEntry>,
    pub timed_out: bool,
    #[ts(type = "number")]
    pub duration_ms: u64,
}

/// Identifies one bounded context line adjacent to a grep match.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ContentGrepContextLine {
    #[ts(type = "number")]
    pub line_number: u64,
    pub line: String,
    pub line_truncated: bool,
}

/// Identifies one matching physical line and its independently retained context.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ContentGrepMatch {
    pub path: String,
    #[ts(type = "number")]
    pub line_number: u64,
    pub line: String,
    pub line_truncated: bool,
    // Defaults preserve ordinary grep responses from agents predating context support.
    #[serde(default)]
    pub before_context: Vec<ContentGrepContextLine>,
    // Defaults preserve ordinary grep responses from agents predating context support.
    #[serde(default)]
    pub after_context: Vec<ContentGrepContextLine>,
}

/// Carries bounded content-search controls while the route identifies the agent and root path.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GrepRequest {
    pub query: String,
    #[serde(default = "default_file_search_timeout_seconds")]
    #[ts(type = "number")]
    pub timeout: u64,
    #[serde(default)]
    pub include_hidden: bool,
    #[serde(default = "default_true")]
    pub respect_gitignore: bool,
    #[serde(default)]
    pub fixed_string: bool,
    #[serde(default)]
    pub case_sensitivity: CaseSensitivity,
    #[serde(default)]
    #[ts(type = "number")]
    pub before_context: u64,
    #[serde(default)]
    #[ts(type = "number")]
    pub after_context: u64,
}

/// Returns matches completed before the grep deadline or supersession signal.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ContentGrepResponse {
    pub results: Vec<ContentGrepMatch>,
    // Distinguishes an old agent's absent context from a genuinely empty context window.
    #[serde(default)]
    pub context_supported: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub truncated: bool,
    #[ts(type = "number")]
    pub omitted_long_lines: u64,
    #[ts(type = "number")]
    pub duration_ms: u64,
}
