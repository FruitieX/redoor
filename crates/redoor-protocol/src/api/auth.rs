//! Browser login and logout request/response bodies.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Carries credentials to the login endpoint without putting secrets in the URL.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// Confirms which configured account now owns the browser session.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LoginResponse {
    pub username: String,
}

/// Confirms that both the browser cookie and its server-side session were removed.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LogoutResponse {
    pub logged_out: bool,
}
