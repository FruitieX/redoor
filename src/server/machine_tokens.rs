//! Durable bearer credentials shared by the MCP transport and management surfaces.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};
use fs2::FileExt;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::task;
use uuid::Uuid;

/// File format version lets later migrations reject an unknown layout instead of corrupting it.
const STORE_VERSION: u32 = 1;
/// Token prefix lets operators distinguish MCP credentials from agent and browser secrets.
const TOKEN_PREFIX: &str = "redoor_mcp";

/// Scopes are defined before device tools exist so all callers share one policy vocabulary.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub(crate) enum MachineScope {
    /// Permits device inventory and status reads.
    #[serde(rename = "devices:read")]
    DevicesRead,
    /// Permits file listing, metadata, reads, searches, and downloads.
    #[serde(rename = "files:read")]
    FilesRead,
    /// Permits file writes, uploads, directory creation, and copy destinations.
    #[serde(rename = "files:write")]
    FilesWrite,
    /// Permits trash, permanent deletion, and move source removal.
    #[serde(rename = "files:delete")]
    FilesDelete,
    /// Permits arbitrary non-PTY command execution as the agent OS account.
    #[serde(rename = "commands:execute")]
    CommandsExecute,
    /// Permits starting and stopping managed agents.
    #[serde(rename = "agents:manage")]
    AgentsManage,
}

impl std::fmt::Display for MachineScope {
    /// Uses the serialized spelling in CLI output and future REST contracts.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = serde_json::to_value(self).map_err(|_| std::fmt::Error)?;
        formatter.write_str(value.as_str().ok_or(std::fmt::Error)?)
    }
}

impl std::str::FromStr for MachineScope {
    type Err = String;

    /// Rejects unknown permission names rather than silently creating an ineffective grant.
    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        serde_json::from_value(serde_json::Value::String(value.to_string()))
            .map_err(|_| format!("unknown MCP scope '{value}'"))
    }
}

/// Stable authorization identity survives credential replacement and future rotation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct MachineGrant {
    pub(crate) id: Uuid,
    pub(crate) owner_id: String,
    pub(crate) scopes: BTreeSet<MachineScope>,
    /// An empty set intentionally grants no device access.
    pub(crate) device_ids: BTreeSet<String>,
    pub(crate) created_at: DateTime<Utc>,
}

/// Replaceable credential stores only a verifier and lifecycle metadata, never its secret.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct MachineCredential {
    pub(crate) id: Uuid,
    pub(crate) grant_id: Uuid,
    pub(crate) name: String,
    verifier: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) expires_at: Option<DateTime<Utc>>,
    pub(crate) revoked_at: Option<DateTime<Utc>>,
    pub(crate) last_used_at: Option<DateTime<Utc>>,
}

/// Safe metadata returned by CLI and later token-management APIs.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct MachineCredentialMetadata {
    pub(crate) credential_id: Uuid,
    pub(crate) grant_id: Uuid,
    pub(crate) owner_id: String,
    pub(crate) name: String,
    pub(crate) scopes: BTreeSet<MachineScope>,
    pub(crate) device_ids: BTreeSet<String>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) expires_at: Option<DateTime<Utc>>,
    pub(crate) revoked_at: Option<DateTime<Utc>>,
    pub(crate) last_used_at: Option<DateTime<Utc>>,
}

/// Authenticated identity passed to policy and audit code without retaining the bearer secret.
#[derive(Clone, Debug)]
pub(crate) struct MachinePrincipal {
    pub(crate) credential_id: Uuid,
    #[allow(dead_code)]
    pub(crate) grant: MachineGrant,
}

/// Creation input stays transport-neutral so CLI and section 3 REST reuse identical validation.
#[derive(Clone, Debug)]
pub(crate) struct CreateMachineToken {
    pub(crate) owner_id: String,
    pub(crate) name: String,
    pub(crate) scopes: BTreeSet<MachineScope>,
    pub(crate) device_ids: BTreeSet<String>,
    pub(crate) expires_at: Option<DateTime<Utc>>,
}

/// Creation returns the secret exactly once beside its safe persisted metadata.
#[derive(Clone, Debug)]
pub(crate) struct CreatedMachineToken {
    pub(crate) secret: String,
    pub(crate) metadata: MachineCredentialMetadata,
}

/// Persistent document is replaced atomically while an inter-process lock is held.
#[derive(Debug, Default, Serialize, Deserialize)]
struct MachineTokenStore {
    version: u32,
    grants: Vec<MachineGrant>,
    credentials: Vec<MachineCredential>,
}

/// Locates durable token state; it deliberately keeps no authorization cache.
#[derive(Clone, Debug)]
pub(crate) struct MachineTokenService {
    path: PathBuf,
    lock_path: PathBuf,
}

/// Administrative CLI locates the same private store as the running server.
#[derive(Args)]
pub(crate) struct MachineTokenArgs {
    #[command(subcommand)]
    command: MachineTokenCommand,
}

/// Minimal bootstrap operations intentionally exclude secret recovery and browser APIs.
#[derive(Subcommand)]
enum MachineTokenCommand {
    /// Create a new grant and print its bearer secret exactly once.
    Create {
        /// Human-readable credential name.
        #[arg(long)]
        name: String,
        /// Stable owner identity used for administrative ownership.
        #[arg(long, default_value = "local-admin")]
        owner: String,
        /// Comma-separated authorization scopes; omit for hello-only access.
        #[arg(long, value_delimiter = ',')]
        scopes: Vec<MachineScope>,
        /// Comma-separated stable device IDs; omit to grant no device access.
        #[arg(long, value_delimiter = ',')]
        devices: Vec<String>,
        /// Optional RFC 3339 expiration timestamp.
        #[arg(long, value_parser = parse_expiry)]
        expires_at: Option<DateTime<Utc>>,
    },
    /// List metadata without secrets or stored verifiers.
    List,
    /// Revoke one credential immediately without deleting its grant identity.
    Revoke {
        /// Credential ID shown by create or list.
        credential_id: Uuid,
    },
}

/// Executes token administration without loading server config or starting a listener.
pub(crate) async fn run_cli(args: MachineTokenArgs) -> Result<()> {
    let service = MachineTokenService::for_application().await?;
    match args.command {
        MachineTokenCommand::Create {
            name,
            owner,
            scopes,
            devices,
            expires_at,
        } => {
            let created = service
                .create(CreateMachineToken {
                    owner_id: owner,
                    name,
                    scopes: scopes.into_iter().collect(),
                    device_ids: devices.into_iter().collect(),
                    expires_at,
                })
                .await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "token": created.secret,
                    "credential": created.metadata,
                }))?
            );
        }
        MachineTokenCommand::List => {
            println!("{}", serde_json::to_string_pretty(&service.list().await?)?)
        }
        MachineTokenCommand::Revoke { credential_id } => {
            if !service.revoke(credential_id).await? {
                bail!("MCP credential {credential_id} was not found");
            }
            println!("Revoked MCP credential {credential_id}");
        }
    }
    Ok(())
}

/// Parses CLI timestamps as UTC while preserving explicit offsets from RFC 3339 input.
fn parse_expiry(value: &str) -> std::result::Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| format!("invalid RFC 3339 expiry: {error}"))
}

impl MachineTokenService {
    /// Uses Redoor's private application data directory rather than source-controlled config.
    pub(crate) async fn for_application() -> Result<Self> {
        let directory = crate::app_name::user_data_directory()?.join("mcp");
        ensure_private_directory(&directory).await?;
        Ok(Self {
            path: directory.join("machine-tokens.json"),
            lock_path: directory.join("machine-tokens.lock"),
        })
    }

    /// Creates a stable grant and one credential while revealing random secret material once.
    pub(crate) async fn create(&self, input: CreateMachineToken) -> Result<CreatedMachineToken> {
        validate_create(&input)?;
        let now = Utc::now();
        let grant = MachineGrant {
            id: Uuid::new_v4(),
            owner_id: input.owner_id,
            scopes: input.scopes,
            device_ids: input.device_ids,
            created_at: now,
        };
        let credential_id = Uuid::new_v4();
        let mut random = [0_u8; 32];
        OsRng.fill_bytes(&mut random);
        let random_secret = URL_SAFE_NO_PAD.encode(random);
        let secret = format!("{TOKEN_PREFIX}_{credential_id}_{random_secret}");
        let credential = MachineCredential {
            id: credential_id,
            grant_id: grant.id,
            name: input.name,
            verifier: verifier(&secret),
            created_at: now,
            expires_at: input.expires_at,
            revoked_at: None,
            last_used_at: None,
        };
        let metadata = metadata(&grant, &credential);
        self.mutate(move |store| {
            store.grants.push(grant);
            store.credentials.push(credential);
            Ok(())
        })
        .await?;
        Ok(CreatedMachineToken { secret, metadata })
    }

    /// Lists only non-secret metadata so verifiers never cross an administrative boundary.
    pub(crate) async fn list(&self) -> Result<Vec<MachineCredentialMetadata>> {
        self.inspect(|store| {
            let mut result = store
                .credentials
                .iter()
                .filter_map(|credential| {
                    store
                        .grants
                        .iter()
                        .find(|grant| grant.id == credential.grant_id)
                        .map(|grant| metadata(grant, credential))
                })
                .collect::<Vec<_>>();
            result.sort_by_key(|record| record.created_at);
            Ok(result)
        })
        .await
    }

    /// Filters metadata at the storage boundary so accounts cannot enumerate each other.
    pub(crate) async fn list_for_owner(
        &self,
        owner_id: String,
    ) -> Result<Vec<MachineCredentialMetadata>> {
        self.inspect(move |store| {
            let mut result = store
                .credentials
                .iter()
                .filter_map(|credential| {
                    store
                        .grants
                        .iter()
                        .find(|grant| grant.id == credential.grant_id && grant.owner_id == owner_id)
                        .map(|grant| metadata(grant, credential))
                })
                .collect::<Vec<_>>();
            result.sort_by_key(|record| record.created_at);
            Ok(result)
        })
        .await
    }

    /// Revokes one replaceable credential while retaining its stable grant record.
    pub(crate) async fn revoke(&self, credential_id: Uuid) -> Result<bool> {
        self.mutate(move |store| {
            let Some(credential) = store
                .credentials
                .iter_mut()
                .find(|item| item.id == credential_id)
            else {
                return Ok(false);
            };
            if credential.revoked_at.is_none() {
                credential.revoked_at = Some(Utc::now());
            }
            Ok(true)
        })
        .await
    }

    /// Revokes only a credential owned by the authenticated browser account.
    pub(crate) async fn revoke_for_owner(
        &self,
        owner_id: String,
        credential_id: Uuid,
    ) -> Result<bool> {
        self.mutate(move |store| {
            let Some(index) = store.credentials.iter().position(|item| {
                item.id == credential_id
                    && store
                        .grants
                        .iter()
                        .any(|grant| grant.id == item.grant_id && grant.owner_id == owner_id)
            }) else {
                return Ok(false);
            };
            if store.credentials[index].revoked_at.is_none() {
                store.credentials[index].revoked_at = Some(Utc::now());
            }
            Ok(true)
        })
        .await
    }

    /// Issues a replacement and revokes its predecessor in one durable transaction.
    pub(crate) async fn rotate_for_owner(
        &self,
        owner_id: String,
        credential_id: Uuid,
    ) -> Result<Option<CreatedMachineToken>> {
        let mut random = [0_u8; 32];
        OsRng.fill_bytes(&mut random);
        let random_secret = URL_SAFE_NO_PAD.encode(random);
        self.mutate(move |store| {
            let Some(old_index) = store.credentials.iter().position(|item| {
                item.id == credential_id
                    && store
                        .grants
                        .iter()
                        .any(|grant| grant.id == item.grant_id && grant.owner_id == owner_id)
            }) else {
                return Ok(None);
            };
            let old = &store.credentials[old_index];
            if old.revoked_at.is_some() || old.expires_at.is_some_and(|expiry| expiry <= Utc::now())
            {
                bail!("only an active MCP credential can be rotated");
            }
            let replacement_id = Uuid::new_v4();
            let secret = format!("{TOKEN_PREFIX}_{replacement_id}_{random_secret}");
            let now = Utc::now();
            let replacement = MachineCredential {
                id: replacement_id,
                grant_id: old.grant_id,
                name: old.name.clone(),
                verifier: verifier(&secret),
                created_at: now,
                expires_at: old.expires_at,
                revoked_at: None,
                last_used_at: None,
            };
            let grant = store
                .grants
                .iter()
                .find(|grant| grant.id == replacement.grant_id)
                .cloned()
                .context("MCP credential grant is missing")?;
            let result = CreatedMachineToken {
                secret,
                metadata: metadata(&grant, &replacement),
            };
            store.credentials.push(replacement);
            store.credentials[old_index].revoked_at = Some(now);
            Ok(Some(result))
        })
        .await
    }

    /// Re-reads durable state and verifies every request so expiry/revocation needs no restart.
    pub(crate) async fn authenticate(&self, secret: &str) -> Result<Option<MachinePrincipal>> {
        let Some(credential_id) = parse_credential_id(secret) else {
            // Hash malformed tokens too, keeping the verifier path less distinguishable to callers.
            let _ = verifier(secret);
            return Ok(None);
        };
        let presented = verifier(secret);
        self.mutate(move |store| {
            let now = Utc::now();
            let Some(credential) = store
                .credentials
                .iter_mut()
                .find(|item| item.id == credential_id)
            else {
                return Ok(None);
            };
            let valid = credential.revoked_at.is_none()
                && credential.expires_at.is_none_or(|expiry| expiry > now)
                && constant_time_equal(&credential.verifier, &presented);
            if !valid {
                return Ok(None);
            }
            credential.last_used_at = Some(now);
            let grant_id = credential.grant_id;
            let Some(grant) = store
                .grants
                .iter()
                .find(|item| item.id == grant_id)
                .cloned()
            else {
                return Ok(None);
            };
            Ok(Some(MachinePrincipal {
                credential_id,
                grant,
            }))
        })
        .await
    }

    /// Runs a read under the same cross-process lock used by mutations for consistent snapshots.
    async fn inspect<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&MachineTokenStore) -> Result<T> + Send + 'static,
    {
        let path = self.path.clone();
        let lock_path = self.lock_path.clone();
        task::spawn_blocking(move || {
            with_locked_store(&path, &lock_path, |store| {
                operation(store).map(|value| (value, false))
            })
        })
        .await
        .context("machine-token storage task failed")?
    }

    /// Serializes a read-modify-write across CLI/server processes and publishes atomically.
    async fn mutate<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut MachineTokenStore) -> Result<T> + Send + 'static,
    {
        let path = self.path.clone();
        let lock_path = self.lock_path.clone();
        task::spawn_blocking(move || {
            with_locked_store(&path, &lock_path, |store| {
                operation(store).map(|value| (value, true))
            })
        })
        .await
        .context("machine-token storage task failed")?
    }
}

/// Validates human-facing metadata before random material is allocated or state changes.
fn validate_create(input: &CreateMachineToken) -> Result<()> {
    if input.name.trim().is_empty() || input.name.len() > 100 {
        bail!("token name must contain 1 to 100 characters");
    }
    if input.owner_id.trim().is_empty() || input.owner_id.len() > 200 {
        bail!("owner identity must contain 1 to 200 characters");
    }
    if input.expires_at.is_some_and(|expiry| expiry <= Utc::now()) {
        bail!("token expiry must be in the future");
    }
    if input
        .device_ids
        .iter()
        .any(|id| id.is_empty() || id.len() > 200)
    {
        bail!("device identifiers must contain 1 to 200 characters");
    }
    Ok(())
}

/// Locks one file for the entire transaction because atomic rename alone cannot prevent lost updates.
fn with_locked_store<T, F>(path: &Path, lock_path: &Path, operation: F) -> Result<T>
where
    F: FnOnce(&mut MachineTokenStore) -> Result<(T, bool)>,
{
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .with_context(|| format!("failed to open MCP token lock {}", lock_path.display()))?;
    lock.lock_exclusive()
        .context("failed to lock MCP token storage")?;
    let mut store = if path.exists() {
        let bytes =
            std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", path.display()))?
    } else {
        MachineTokenStore {
            version: STORE_VERSION,
            ..MachineTokenStore::default()
        }
    };
    if store.version != STORE_VERSION {
        bail!("unsupported MCP token store version {}", store.version);
    }
    let (value, changed) = operation(&mut store)?;
    if changed {
        write_store(path, &store)?;
    }
    FileExt::unlock(&lock).context("failed to unlock MCP token storage")?;
    Ok(value)
}

/// Writes a private temporary file and renames it so readers never observe partial JSON.
fn write_store(path: &Path, store: &MachineTokenStore) -> Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(store)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(&temporary, &bytes)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

/// Creates credential storage with owner-only traversal permissions on supported platforms.
async fn ensure_private_directory(path: &Path) -> Result<()> {
    tokio::fs::create_dir_all(path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(())
}

/// Hashes the complete opaque token so stored state cannot authenticate if copied as a bearer value.
fn verifier(secret: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()))
}

/// Uses fixed-time comparison for equal-length SHA-256 verifier encodings.
fn constant_time_equal(expected: &str, presented: &str) -> bool {
    expected.as_bytes().ct_eq(presented.as_bytes()).into()
}

/// Extracts the indexed credential identifier without accepting another token namespace.
fn parse_credential_id(secret: &str) -> Option<Uuid> {
    let rest = secret.strip_prefix(&format!("{TOKEN_PREFIX}_"))?;
    let (id, random) = rest.split_once('_')?;
    (!random.is_empty())
        .then(|| Uuid::parse_str(id).ok())
        .flatten()
}

/// Joins a credential to its stable grant while deliberately omitting the verifier.
fn metadata(grant: &MachineGrant, credential: &MachineCredential) -> MachineCredentialMetadata {
    MachineCredentialMetadata {
        credential_id: credential.id,
        grant_id: grant.id,
        owner_id: grant.owner_id.clone(),
        name: credential.name.clone(),
        scopes: grant.scopes.clone(),
        device_ids: grant.device_ids.clone(),
        created_at: credential.created_at,
        expires_at: credential.expires_at,
        revoked_at: credential.revoked_at,
        last_used_at: credential.last_used_at,
    }
}
