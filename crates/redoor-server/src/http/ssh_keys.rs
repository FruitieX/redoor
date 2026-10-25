//! REST endpoints for per-device SSH keys and pinned host keys.
//!
//! Session-only, like the other agent configuration routes, because they carry login
//! passwords and change what the server can log in to.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use axum::{Json, extract::State as AxumState, http::StatusCode};
use redoor_core::config::{AgentConfig, SshBackedAgentConfig, edit_ssh_agent};
use redoor_core::log;
use redoor_protocol::api::ssh_keys::{
    EnrollSshKeyRequest, PinSshHostKeyRequest, SshHostKeyPinResponse, SshHostKeyResponse,
    SshKeyEnrollmentResponse, SshKeyRemovalResponse,
};
use redoor_protocol::ids::AgentId;
use redoor_protocol::wire::log::Level;
use zeroize::Zeroizing;

use super::agent_configuration::{
    find_ssh_agent, replace_runtime_agent, stop_for_configuration_change, with_name_reserved,
};
use super::{access::ResolvedDevice, error::ApiError, state::ServerState};
use crate::ssh::keys::ManagedKey;
use crate::ssh::managed_keys::{ManagedKeyDevice, ManagedKeyError, PendingKey};

/// Route: `GET /api/v1/agents/{agent}/ssh-host-key` probes the host key the device presents
/// so the user can confirm it before any credential is sent.
pub(crate) async fn get_host_key_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<SshHostKeyResponse>, ApiError> {
    // A probe writes temporary state too; edits or deletion must not race that write.
    let _operation = state.ssh_key_operations.begin(&agent_id)?;
    let device = managed_key_device(&state, &agent_id).await?;
    let probed = device.probe_host_key().await?;
    Ok(Json(SshHostKeyResponse {
        key_type: probed.key.key_type(),
        fingerprint: probed.key.fingerprint(),
        previously_trusted: probed.previously_trusted,
        pinned_fingerprint: probed.pinned.map(|key| key.fingerprint()),
    }))
}

/// Route: `POST /api/v1/agents/{agent}/ssh-host-key/pin` replaces the pinned host key after
/// the user confirmed a host key change. Never done automatically.
pub(crate) async fn pin_host_key_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    AxumState(state): AxumState<ServerState>,
    Json(request): Json<PinSshHostKeyRequest>,
) -> Result<Json<SshHostKeyPinResponse>, ApiError> {
    let operation = state.ssh_key_operations.begin(&agent_id)?;
    complete_operation(async move {
        let _operation = operation;
        let device = managed_key_device(&state, &agent_id).await?;
        let key = device.pin_host_key(&request.host_key_fingerprint).await?;
        Ok(Json(SshHostKeyPinResponse {
            key_type: key.key_type(),
            fingerprint: key.fingerprint(),
        }))
    })
    .await
}

/// Route: `POST /api/v1/agents/{agent}/ssh-key` sets up the device's managed key, or rotates
/// it when the device already has one.
///
/// The new key becomes current only after it is installed and verified, so a failure at any
/// earlier step leaves the device as it was.
pub(crate) async fn enroll_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    AxumState(state): AxumState<ServerState>,
    Json(request): Json<EnrollSshKeyRequest>,
) -> Result<Json<SshKeyEnrollmentResponse>, ApiError> {
    let operation = state.ssh_key_operations.begin(&agent_id)?;
    complete_operation(async move {
        let _operation = operation;
        enroll(&state, &agent_id, request).await.map(Json)
    })
    .await
}

/// The enrollment itself, kept out of the `complete_operation` closure so rustfmt formats it.
async fn enroll(
    state: &ServerState,
    agent_id: &AgentId,
    request: EnrollSshKeyRequest,
) -> Result<SshKeyEnrollmentResponse, ApiError> {
    let config = find_ssh_agent(state, agent_id).await?;
    let device = ManagedKeyDevice::new(config.clone(), state.ssh_keys.clone())
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    // Wrapped right away so the typed password is wiped when the enrollment drops it.
    let password = request
        .password
        .filter(|password| !password.is_empty())
        .map(Zeroizing::new);
    let rotated = config.managed_ssh_key;
    log!(
        Level::Info,
        "Setting up managed SSH key: device={}, rotation={}, typed_password={}",
        agent_id,
        rotated,
        password.is_some()
    );
    let pending = device
        .install_new_key(request.host_key_fingerprint.as_deref(), password)
        .await?;
    let fingerprint = pending.key().fingerprint();
    if rotated {
        let new_key = match commit_rotation(state, agent_id, &config, &device, &pending).await {
            Ok(key) => key,
            Err(error) => {
                device.rollback(pending).await;
                return Err(error);
            }
        };
        // The bootstrap credential is no longer needed once the new key is current.
        drop(pending);
        device.finish_rotation(&new_key).await;
    } else {
        let runtime_error =
            match commit_key(state, agent_id, &config, Some((&device, &pending))).await {
                Ok(error) => error,
                Err(error) => {
                    device.rollback(pending).await;
                    return Err(error);
                }
            };
        drop(pending);
        if let Some(error) = runtime_error {
            return Err(saved_but_not_registered("The SSH key was set up", error));
        }
    }
    log!(
        Level::Info,
        "Managed SSH key in use: device={}, fingerprint={}",
        agent_id,
        fingerprint
    );
    Ok(SshKeyEnrollmentResponse {
        fingerprint,
        rotated,
    })
}

/// Route: `DELETE /api/v1/agents/{agent}/ssh-key` removes the key from the device when it
/// can be reached, then always from the config and locally. The pinned host key stays, so
/// setting up a key again does not ask for confirmation.
pub(crate) async fn remove_handler(
    ResolvedDevice(agent_id): ResolvedDevice,
    AxumState(state): AxumState<ServerState>,
) -> Result<Json<SshKeyRemovalResponse>, ApiError> {
    let operation = state.ssh_key_operations.begin(&agent_id)?;
    complete_operation(async move {
        let _operation = operation;
        remove_key(&state, &agent_id).await.map(Json)
    })
    .await
}

/// The removal itself, kept out of the `complete_operation` closure so rustfmt formats it.
async fn remove_key(
    state: &ServerState,
    agent_id: &AgentId,
) -> Result<SshKeyRemovalResponse, ApiError> {
    let config = find_ssh_agent(state, agent_id).await?;
    if !config.managed_ssh_key {
        return Err(ApiError::not_found(format!(
            "Device '{agent_id}' has no managed SSH key"
        )));
    }
    let device = ManagedKeyDevice::new(config.clone(), state.ssh_keys.clone())
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    // Persist first: a config write failure must not revoke the still-configured key.
    let runtime_error = commit_key(state, agent_id, &config, None).await?;
    let device_error = device.remove_from_device().await;
    if let Some(error) = &device_error {
        redoor_core::log_failure!(
            Level::Warn,
            "Could not remove the managed SSH key from the device; deleting it locally anyway: device={}, error={}",
            agent_id,
            error
        );
    }
    // The config stops using the key before its files go away.
    state
        .ssh_keys
        .delete_keys(agent_id)
        .await
        .map_err(|error| {
            ApiError::internal(format!("Failed to delete the local SSH key: {error}"))
        })?;
    if let Some(error) = runtime_error {
        return Err(saved_but_not_registered("The SSH key was removed", error));
    }
    Ok(SshKeyRemovalResponse {
        removed_from_device: device_error.is_none(),
        device_error,
    })
}

/// Explains a failure that happened after the config was already written, so the user does
/// not read it as "nothing changed" and retry an operation that in fact succeeded.
fn saved_but_not_registered(done: &str, error: ApiError) -> ApiError {
    ApiError::status(
        error.status_code(),
        format!(
            "{done} and the config was saved, but the device could not be registered again: {error}. Restart the server to load the device."
        ),
    )
}

/// Loads the device's config entry for a key operation.
async fn managed_key_device(
    state: &ServerState,
    agent_id: &AgentId,
) -> Result<ManagedKeyDevice, ApiError> {
    let config = find_ssh_agent(state, agent_id).await?;
    ManagedKeyDevice::new(config, state.ssh_keys.clone())
        .map_err(|error| ApiError::bad_request(error.to_string()))
}

/// Commits a first key (`first_key` is `Some`) or a removal (`None`) to the config: sets
/// `managed_ssh_key` accordingly and always drops the stored password. A device uses either a
/// password or a managed key, never both, and keeping the password would also re-enable
/// askpass, which contradicts the managed-key policy.
///
/// A first key is renamed into place (and the rename synced) before the config write, so a
/// crash in between leaves at worst an unused key file, never a config that claims a key
/// that does not exist. If the config write fails, the key file is deleted again and the
/// caller rolls back the remote line; a crash leftover is overwritten by the next setup.
///
/// The config edit lock is held only for this write, not the whole flow, because it blocks
/// device starts and the flow includes network round trips. The device's target and
/// username are checked again under it, because a key installed for one account must not be
/// recorded for another.
/// An outer error means persistence did not happen. A runtime error is returned inside
/// `Ok` so callers preserve the durably referenced key and finish cleanup before reporting it.
async fn commit_key(
    state: &ServerState,
    agent_id: &AgentId,
    enrolled_for: &SshBackedAgentConfig,
    first_key: Option<(&ManagedKeyDevice, &PendingKey)>,
) -> Result<Option<ApiError>, ApiError> {
    let _edit_guard = state.config_edit_lock.lock().await;
    let mut config = find_ssh_agent(state, agent_id).await?;
    if config.target != enrolled_for.target || config.username != enrolled_for.username {
        return Err(ApiError::conflict(
            "The device's target or username changed during the SSH key setup",
        ));
    }
    config.managed_ssh_key = first_key.is_some();
    config.password = None;
    let agent_config = AgentConfig::SshBacked(config.clone());
    let name = agent_config.effective_name();
    with_name_reserved(state, agent_id, &name, async {
        stop_for_configuration_change(state, agent_id).await?;
        if let Some((device, pending)) = first_key {
            device.commit(pending).await.map_err(|error| {
                ApiError::internal(format!("Failed to save the new SSH key: {error}"))
            })?;
        }
        if let Err(error) = edit_ssh_agent(&state.config_path, agent_id, Some(&config)).await {
            if first_key.is_some() {
                discard_uncommitted_key(state, agent_id).await;
            }
            return Err(ApiError::internal(error.to_string()));
        }
        // Once persistence succeeds, this key must survive even if the runtime cannot
        // register it. Return that error separately so callers finish cleanup without rollback.
        Ok(replace_runtime_agent(state, agent_id, agent_config)
            .await
            .err())
    })
    .await
}

/// Deletes a first key that was renamed into place but never recorded in the config, so no
/// private key stays around for a device that does not use it. Logged instead of returned:
/// the config write error is what the user needs to see, and the next setup overwrites a
/// leftover anyway.
async fn discard_uncommitted_key(state: &ServerState, agent_id: &AgentId) {
    if let Err(error) = state.ssh_keys.delete_keys(agent_id).await {
        redoor_core::log_failure!(
            Level::Warn,
            "Failed to delete an SSH key whose config write failed: device={}, error={}",
            agent_id,
            error
        );
    }
}

/// Makes a rotated key current with one atomic file rename and no config write.
///
/// The config already says the device uses its managed key and every agent start loads the
/// key file again, so the next start picks up the new key. Skipping the config write means a
/// connected agent is not stopped; its ssh session authenticated already and keeps working.
/// The config edit lock is held only to check, like [`commit_key`], that the device was not
/// pointed at another account or had its key removed meanwhile.
async fn commit_rotation(
    state: &ServerState,
    agent_id: &AgentId,
    enrolled_for: &SshBackedAgentConfig,
    device: &ManagedKeyDevice,
    pending: &PendingKey,
) -> Result<ManagedKey, ApiError> {
    let _edit_guard = state.config_edit_lock.lock().await;
    let config = find_ssh_agent(state, agent_id).await?;
    if config.target != enrolled_for.target
        || config.username != enrolled_for.username
        || !config.managed_ssh_key
    {
        return Err(ApiError::conflict(
            "The device's target, username or SSH key changed during the SSH key rotation",
        ));
    }
    device.commit(pending).await.map_err(|error| {
        ApiError::internal(format!("Failed to switch to the new SSH key: {error}"))
    })
}

/// Lets mutations finish after a browser disconnects, keeping their busy guard and
/// bootstrap credential alive until commit or rollback and all local cleanup complete.
pub(super) async fn complete_operation<T: Send + 'static>(
    operation: impl std::future::Future<Output = Result<T, ApiError>> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::spawn(operation)
        .await
        .map_err(|error| ApiError::internal(format!("SSH key operation task failed: {error}")))?
}

impl From<ManagedKeyError> for ApiError {
    /// Maps each failure to the status the UI needs to choose its message.
    fn from(error: ManagedKeyError) -> Self {
        match &error {
            ManagedKeyError::Input(_) => ApiError::bad_request(error.to_string()),
            ManagedKeyError::HostKeyMismatch(_) => ApiError::conflict(error.to_string()),
            ManagedKeyError::Remote(_) => {
                ApiError::status(StatusCode::BAD_GATEWAY, error.to_string())
            }
            ManagedKeyError::Local(_) => ApiError::internal(error.to_string()),
        }
    }
}

/// Devices with a key or host key operation in progress.
///
/// Enrollment, rotation, removal, re-pinning and device edits of one device would race over
/// `authorized_keys`, the key directory and the config, so a second request for the same
/// device gets 409 instead of waiting behind network round trips.
#[derive(Clone, Default)]
pub(crate) struct SshKeyOperations {
    /// Ids of the devices that are busy.
    busy: Arc<Mutex<HashSet<AgentId>>>,
}

impl SshKeyOperations {
    /// Marks the device busy until the returned guard is dropped, or fails with 409.
    pub(crate) fn begin(&self, agent_id: &AgentId) -> Result<SshKeyOperation, ApiError> {
        let mut busy = self
            .busy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !busy.insert(agent_id.clone()) {
            return Err(ApiError::conflict(format!(
                "An SSH key operation is already running for device '{agent_id}'"
            )));
        }
        Ok(SshKeyOperation {
            busy: self.busy.clone(),
            agent_id: agent_id.clone(),
        })
    }
}

/// Keeps one device marked busy while alive.
pub(crate) struct SshKeyOperation {
    /// Shared set the device is removed from on drop.
    busy: Arc<Mutex<HashSet<AgentId>>>,
    /// The busy device.
    agent_id: AgentId,
}

impl Drop for SshKeyOperation {
    /// Frees the device, also when the request is cancelled mid-flight.
    fn drop(&mut self) {
        self.busy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.agent_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lost HTTP waiter must neither interrupt cleanup nor release the per-device guard.
    #[tokio::test]
    async fn cancelled_waiter_keeps_operation_alive_until_cleanup() {
        let operations = SshKeyOperations::default();
        let device = AgentId::generate();
        let operation = operations.begin(&device).unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let (cleaned_tx, cleaned_rx) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(complete_operation(async move {
            started_tx.send(()).unwrap();
            finish_rx.await.unwrap();
            drop(operation);
            cleaned_tx.send(()).unwrap();
            Ok(())
        }));
        started_rx.await.unwrap();
        waiter.abort();
        let _ = waiter.await;
        // The detached mutation still owns the busy guard while cleanup is pending.
        assert!(operations.begin(&device).is_err());
        finish_tx.send(()).unwrap();
        cleaned_rx.await.unwrap();
        // A new request may proceed only after the previous mutation has finished cleanup.
        assert!(operations.begin(&device).is_ok());
    }
}
