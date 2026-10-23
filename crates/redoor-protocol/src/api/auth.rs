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

/// Tells the public login page which sign-in methods to offer before any session exists.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LoginMethodsResponse {
    /// Present when the server is configured for OpenID Connect sign-in.
    pub oidc: Option<OidcLoginMethod>,
}

/// What the login page needs to lead with the identity provider button.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OidcLoginMethod {
    /// Operator-chosen label, shown as "Sign in with {provider_name}".
    pub provider_name: String,
}
