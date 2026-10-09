//! Runtime infrastructure shared by the agent, server, CLI, and binary crates.
//!
//! Holds everything more than one role needs (logging, configuration, safe filesystem
//! helpers, process identity) so the role crates can stay independent of each other.

pub mod app_name;
pub mod atomic_rename;
pub mod config;
pub mod desktop;
pub mod destination;
pub mod identity;
pub mod logging;
pub mod process;
pub mod safe_fs;
pub mod server_address;
pub mod systemd_notify;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod websocket;
