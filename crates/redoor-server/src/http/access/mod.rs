//! Access control shared by REST and MCP: the principal, its single decision function, the
//! route access table and the middleware that applies it.

mod middleware;
mod principal;
pub(crate) mod routes;

pub(crate) use middleware::{access_control, bearer_token};
pub(crate) use principal::{Principal, TokenGrant};
