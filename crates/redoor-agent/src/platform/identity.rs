//! Identity of the host and agent process reported at registration and in agent details.

use sysinfo::System;

/// Host and process identity gathered from portable OS APIs.
pub(crate) struct HostIdentity {
    pub(crate) os: String,
    pub(crate) arch: String,
    pub(crate) hostname: String,
    pub(crate) username: String,
    /// Effective UID, which stays correct under systemd `User=` unlike `$USER`.
    pub(crate) uid: u32,
    /// Derived from the effective UID, never from the username.
    pub(crate) is_root: bool,
}

impl HostIdentity {
    /// Reads the current identity; registration and details share it so they cannot disagree.
    pub(crate) fn current() -> Self {
        let effective_uid = nix::unistd::Uid::effective();
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            hostname: System::host_name().unwrap_or_else(|_| "unknown".to_string()),
            username: std::env::var("USER").unwrap_or_else(|_| "unknown".to_string()),
            uid: effective_uid.as_raw(),
            is_root: effective_uid.is_root(),
        }
    }
}
