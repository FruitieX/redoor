//! Host user and group enumeration for root-only ownership editors.

use redoor_protocol::api::agents::AgentAccountsResponse;

/// Enumerates users and groups off the async runtime because NSS iteration blocks.
pub(crate) async fn list() -> Result<AgentAccountsResponse, String> {
    tokio::task::spawn_blocking(|| crate::ownership::with_nss_lock(nss::enumerate_locked))
        .await
        .map_err(|error| format!("Account enumeration worker failed: {error}"))?
}

/// NSS-backed enumeration for glibc and macOS libc.
#[cfg(not(target_os = "android"))]
mod nss {
    use redoor_protocol::api::agents::{
        AgentAccountGroup, AgentAccountUser, AgentAccountsResponse,
    };

    /// Caps NSS dumps so an LDAP host cannot serialize an unbounded catalog into one response.
    const MAX_ENUMERATED_ACCOUNTS: usize = 8192;

    /// Closes the passwd iterator even when UTF-8 skipping or the cap aborts enumeration early.
    struct PasswdEnumerationGuard;

    impl Drop for PasswdEnumerationGuard {
        /// Releases libc's process-global passwd cursor.
        fn drop(&mut self) {
            unsafe {
                nix::libc::endpwent();
            }
        }
    }

    /// Closes the group iterator even when UTF-8 skipping or the cap aborts enumeration early.
    struct GroupEnumerationGuard;

    impl Drop for GroupEnumerationGuard {
        /// Releases libc's process-global group cursor.
        fn drop(&mut self) {
            unsafe {
                nix::libc::endgrent();
            }
        }
    }

    /// Runs only while the process-wide NSS lock is held so getpwent cannot interleave with lookups.
    pub(super) fn enumerate_locked() -> Result<AgentAccountsResponse, String> {
        let mut users = Vec::new();
        let mut groups = Vec::new();
        let mut truncated = false;

        unsafe {
            nix::libc::setpwent();
            let _passwd_guard = PasswdEnumerationGuard;
            loop {
                let passwd = nix::libc::getpwent();
                if passwd.is_null() {
                    break;
                }
                if users.len() >= MAX_ENUMERATED_ACCOUNTS {
                    truncated = true;
                    break;
                }
                let name = match std::ffi::CStr::from_ptr((*passwd).pw_name).to_str() {
                    Ok(name) => name.to_string(),
                    // Lossy labels would chown the wrong account, so skip non-UTF-8 names.
                    Err(_) => continue,
                };
                users.push(AgentAccountUser {
                    name,
                    uid: (*passwd).pw_uid,
                });
            }
        }

        unsafe {
            nix::libc::setgrent();
            let _group_guard = GroupEnumerationGuard;
            loop {
                let group = nix::libc::getgrent();
                if group.is_null() {
                    break;
                }
                if groups.len() >= MAX_ENUMERATED_ACCOUNTS {
                    truncated = true;
                    break;
                }
                let name = match std::ffi::CStr::from_ptr((*group).gr_name).to_str() {
                    Ok(name) => name.to_string(),
                    Err(_) => continue,
                };
                groups.push(AgentAccountGroup {
                    name,
                    gid: (*group).gr_gid,
                });
            }
        }

        users.sort_by(|left, right| left.name.cmp(&right.name));
        groups.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(AgentAccountsResponse {
            users,
            groups,
            truncated,
        })
    }
}

/// Bionic has no getpwent/getgrent, so owner/group selects stay empty on Android.
#[cfg(target_os = "android")]
mod nss {
    use redoor_protocol::api::agents::AgentAccountsResponse;

    /// Reports an empty, complete catalog.
    pub(super) fn enumerate_locked() -> Result<AgentAccountsResponse, String> {
        Ok(AgentAccountsResponse {
            users: Vec::new(),
            groups: Vec::new(),
            truncated: false,
        })
    }
}
