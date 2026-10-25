//! Bodies for the per-device SSH key and pinned host key endpoints.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The host key a device presented to a probe, shown to the user for confirmation before
/// any credential is sent to the device.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SshHostKeyResponse {
    /// OpenSSH key type, for example `ssh-ed25519`.
    pub key_type: String,
    /// `SHA256:` fingerprint in the format `ssh-keygen -l` prints, so users can compare it
    /// with the value shown on the device itself.
    pub fingerprint: String,
    /// Whether the server user's own known_hosts files already trust this exact key for the
    /// host. Informational only: the user still confirms.
    pub previously_trusted: bool,
    /// Fingerprint of the key pinned for the device, when there is one; differing from
    /// `fingerprint` means the host key changed since it was pinned.
    pub pinned_fingerprint: Option<String>,
}

/// Starts or rotates the device's managed key. Sent only by browser sessions.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnrollSshKeyRequest {
    /// Fingerprint the user confirmed. Required when no host key is pinned yet; the server
    /// refuses to continue when the device now presents a different key.
    pub host_key_fingerprint: Option<String>,
    /// Login password used once to install the key. Never stored or logged.
    pub password: Option<String>,
}

/// Result of a successful enrollment or rotation.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SshKeyEnrollmentResponse {
    /// `SHA256:` fingerprint of the new public key, as it appears in the device's logs. It
    /// also identifies the key: the device's `authorized_keys` line carries it in its
    /// `redoor:<device-id>:<fingerprint>` marker.
    pub fingerprint: String,
    /// Whether an existing managed key was replaced instead of a first key created.
    pub rotated: bool,
}

/// Result of removing the managed key.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SshKeyRemovalResponse {
    /// Whether the key's `authorized_keys` line was removed from the device. When false the
    /// key was still deleted locally, but the line is left on the device.
    pub removed_from_device: bool,
    /// Why the device could not be cleaned up, when it could not.
    pub device_error: Option<String>,
}

/// Re-pins the device's host key after the user confirmed a change.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PinSshHostKeyRequest {
    /// Fingerprint the user confirmed; the device must still present exactly this key.
    pub host_key_fingerprint: String,
}

/// The host key pinned for the device after a re-pin.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SshHostKeyPinResponse {
    /// OpenSSH key type, for example `ssh-ed25519`.
    pub key_type: String,
    /// `SHA256:` fingerprint of the pinned key.
    pub fingerprint: String,
}
