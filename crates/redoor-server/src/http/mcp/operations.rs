//! Durable principal-owned operation lifecycle and quota-bounded command output.

use anyhow::{Context, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::Mutex,
};
use uuid::Uuid;

/// Default retained output per operation, keeping one chatty command from filling the store.
const DEFAULT_JOB_QUOTA: usize = 1024 * 1024;
/// Default retained output across all operations, bounding total disk and memory use.
const DEFAULT_GLOBAL_QUOTA: usize = 64 * 1024 * 1024;
/// Default age after which finished operations are dropped so history does not grow forever.
const DEFAULT_EXPIRY_SECONDS: u64 = 24 * 60 * 60;

/// One retained operation is addressable only through the access token that admitted it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct OperationRecord {
    pub(super) id: Uuid,
    pub(super) owner_token: Uuid,
    pub(super) device_ids: Vec<String>,
    pub(super) kind: String,
    pub(super) state: String,
    pub(super) output_size: usize,
    pub(super) output_truncated: bool,
    pub(super) terminal: Option<Value>,
    pub(super) remote_id: Option<u64>,
    pub(super) cancel_requested: bool,
    pub(super) created_at: Timestamp,
    pub(super) updated_at: Timestamp,
    #[serde(skip)]
    pub(super) spool_path: PathBuf,
}

/// Idempotency entries bind a principal key to the complete normalized mutation.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct IdempotencyRecord {
    fingerprint: String,
    result: Value,
}

/// The durable document keeps admissions and retry decisions across server restarts.
#[derive(Default, Serialize, Deserialize)]
struct PersistedOperations {
    records: Vec<OperationRecord>,
    idempotency: HashMap<String, IdempotencyRecord>,
}

/// Process-local locks protect a durable single-server operation registry.
#[derive(Clone)]
pub(super) struct OperationRegistry {
    records: Arc<Mutex<HashMap<Uuid, Arc<Mutex<OperationRecord>>>>>,
    idempotency: Arc<Mutex<HashMap<String, IdempotencyRecord>>>,
    admission_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    directory: Arc<PathBuf>,
    metadata_path: Arc<PathBuf>,
    io_lock: Arc<Mutex<()>>,
    per_job_quota: usize,
    global_quota: usize,
    expiry: Duration,
}

impl OperationRegistry {
    /// Loads durable admissions from the given directory and marks unacknowledged work
    /// interrupted after restart; tests pass an isolated directory.
    pub(super) async fn in_directory(directory: PathBuf) -> Result<Self> {
        tokio::fs::create_dir_all(&directory)
            .await
            .context("creating MCP operation directory")?;
        #[cfg(unix)]
        tokio::fs::set_permissions(
            &directory,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .await
        .context("protecting MCP operation directory")?;
        let metadata_path = directory.join("operations.json");
        let persisted: PersistedOperations = match tokio::fs::read(&metadata_path).await {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("reading MCP operation metadata")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                PersistedOperations::default()
            }
            Err(error) => return Err(error).context("reading MCP operation metadata"),
        };
        let expiry = Duration::from_secs(environment_usize(
            "REDOOR_MCP_OPERATION_EXPIRY_SECONDS",
            DEFAULT_EXPIRY_SECONDS as usize,
        ) as u64);
        let cutoff = Timestamp::now().checked_sub(expiry)?;
        let mut records = HashMap::new();
        for mut record in persisted.records {
            if record.updated_at < cutoff {
                continue;
            }
            // Router counters belong to one process and must never survive recovery.
            record.remote_id = None;
            record.spool_path = directory.join(format!("{}.output", record.id));
            if matches!(record.state.as_str(), "admitted" | "running" | "canceling") {
                record.state = "interrupted".to_string();
                record.terminal = Some(
                    serde_json::json!({"reason":"server_restarted_before_terminal_acknowledgement"}),
                );
                record.updated_at = Timestamp::now();
            }
            records.insert(record.id, Arc::new(Mutex::new(record)));
        }
        let registry = Self {
            records: Arc::new(Mutex::new(records)),
            idempotency: Arc::new(Mutex::new(persisted.idempotency)),
            admission_locks: Arc::default(),
            directory: Arc::new(directory),
            metadata_path: Arc::new(metadata_path),
            io_lock: Arc::default(),
            per_job_quota: environment_usize("REDOOR_MCP_OPERATION_JOB_QUOTA", DEFAULT_JOB_QUOTA),
            global_quota: environment_usize(
                "REDOOR_MCP_OPERATION_GLOBAL_QUOTA",
                DEFAULT_GLOBAL_QUOTA,
            ),
            expiry,
        };
        registry.expire().await?;
        registry.persist().await?;
        Ok(registry)
    }

    /// Returns the configured hard ceiling for one command's retained output.
    pub(super) fn per_job_quota(&self) -> usize {
        self.per_job_quota
    }

    /// Creates and durably records admission before the caller starts remote work.
    pub(super) async fn create(
        &self,
        owner_token: Uuid,
        device_ids: Vec<String>,
        kind: impl Into<String>,
    ) -> Result<Arc<Mutex<OperationRecord>>> {
        self.expire().await?;
        let id = Uuid::new_v4();
        let now = Timestamp::now();
        let record = Arc::new(Mutex::new(OperationRecord {
            id,
            owner_token,
            device_ids,
            kind: kind.into(),
            state: "admitted".to_string(),
            output_size: 0,
            output_truncated: false,
            terminal: None,
            remote_id: None,
            cancel_requested: false,
            created_at: now,
            updated_at: now,
            spool_path: self.directory.join(format!("{id}.output")),
        }));
        self.records.lock().await.insert(id, record.clone());
        self.persist().await?;
        Ok(record)
    }

    /// Looks up an unexpired operation without disclosing ownership.
    pub(super) async fn get(&self, id: Uuid) -> Result<Option<Arc<Mutex<OperationRecord>>>> {
        self.expire().await?;
        Ok(self.records.lock().await.get(&id).cloned())
    }

    /// Persists state transitions after a remote acknowledgement or cancellation request.
    pub(super) async fn update(&self, record: &Arc<Mutex<OperationRecord>>) -> Result<()> {
        record.lock().await.updated_at = Timestamp::now();
        self.persist().await
    }

    /// Appends only quota-admitted bytes while continuing to drain discarded output.
    pub(super) async fn append_output(
        &self,
        record: &Arc<Mutex<OperationRecord>>,
        data: &[u8],
        requested_limit: usize,
    ) -> Result<()> {
        let _guard = self.io_lock.lock().await;
        let global_used = self.global_usage().await;
        let mut current = record.lock().await;
        let allowed = data
            .len()
            .min(
                requested_limit
                    .min(self.per_job_quota)
                    .saturating_sub(current.output_size),
            )
            .min(self.global_quota.saturating_sub(global_used));
        current.output_truncated |= allowed < data.len();
        current.output_size += allowed;
        current.updated_at = Timestamp::now();
        let path = current.spool_path.clone();
        drop(current);
        if allowed > 0 {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await?;
            file.write_all(&data[..allowed]).await?;
        }
        drop(_guard);
        self.persist().await
    }

    /// Reads only the requested output page from disk.
    pub(super) async fn read_output(
        &self,
        record: &OperationRecord,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<u8>> {
        let length = record.output_size.saturating_sub(offset).min(limit);
        if length == 0 {
            return Ok(Vec::new());
        }
        let _guard = self.io_lock.lock().await;
        let mut file = tokio::fs::File::open(&record.spool_path).await?;
        file.seek(std::io::SeekFrom::Start(offset as u64)).await?;
        let mut bytes = vec![0; length];
        file.read_exact(&mut bytes).await?;
        Ok(bytes)
    }

    /// Reserves one principal's retry key through admission and durable result publication.
    pub(super) async fn lock_admission(
        &self,
        owner: Uuid,
        key: &str,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let mut locks = self.admission_locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        let lock = locks
            .entry(idempotency_key(owner, key))
            .or_default()
            .upgrade()
            .unwrap_or_else(|| {
                let lock = Arc::new(Mutex::new(()));
                locks.insert(idempotency_key(owner, key), Arc::downgrade(&lock));
                lock
            });
        drop(locks);
        lock.lock_owned().await
    }

    /// Returns a prior idempotent result only for an identical normalized request.
    pub(super) async fn idempotent_result(
        &self,
        owner: Uuid,
        key: &str,
        fingerprint: &str,
    ) -> std::result::Result<Option<Value>, ()> {
        let records = self.idempotency.lock().await;
        match records.get(&idempotency_key(owner, key)) {
            Some(record) if record.fingerprint == fingerprint => Ok(Some(record.result.clone())),
            Some(_) => Err(()),
            None => Ok(None),
        }
    }

    /// Durably publishes a successful admission result for safe retries.
    pub(super) async fn remember(
        &self,
        owner: Uuid,
        key: String,
        fingerprint: String,
        result: Value,
    ) -> Result<()> {
        self.idempotency.lock().await.insert(
            idempotency_key(owner, &key),
            IdempotencyRecord {
                fingerprint,
                result,
            },
        );
        self.persist().await
    }

    /// Removes expired records and their private spools without touching active work.
    async fn expire(&self) -> Result<()> {
        let cutoff = Timestamp::now().checked_sub(self.expiry)?;
        let entries = self
            .records
            .lock()
            .await
            .iter()
            .map(|(id, record)| (*id, record.clone()))
            .collect::<Vec<_>>();
        let mut expired = Vec::new();
        for (id, record) in entries {
            let record = record.lock().await;
            if record.updated_at < cutoff
                && !matches!(record.state.as_str(), "admitted" | "running" | "canceling")
            {
                expired.push((id, record.spool_path.clone()));
            }
        }
        if expired.is_empty() {
            return Ok(());
        }
        let mut records = self.records.lock().await;
        for (id, path) in expired {
            records.remove(&id);
            let _ = tokio::fs::remove_file(path).await;
        }
        Ok(())
    }

    /// Replaces the metadata document atomically while output bytes remain separate.
    async fn persist(&self) -> Result<()> {
        let _guard = self.io_lock.lock().await;
        let entries = self
            .records
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut records = Vec::with_capacity(entries.len());
        for record in entries {
            records.push(record.lock().await.clone());
        }
        let document = PersistedOperations {
            records,
            idempotency: self.idempotency.lock().await.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&document)?;
        let temporary = self
            .metadata_path
            .with_extension(format!("tmp-{}", Uuid::new_v4()));
        tokio::fs::write(&temporary, bytes).await?;
        tokio::fs::rename(&temporary, self.metadata_path.as_ref()).await?;
        Ok(())
    }

    /// Sums bounded record sizes without reading spool contents.
    async fn global_usage(&self) -> usize {
        let entries = self
            .records
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut total = 0usize;
        for record in entries {
            total = total.saturating_add(record.lock().await.output_size);
        }
        total
    }
}

/// Parses positive byte/time limits while retaining safe defaults for invalid values.
fn environment_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
/// Names idempotency entries without exposing bearer credential identifiers.
fn idempotency_key(owner: Uuid, key: &str) -> String {
    format!("{owner}:{key}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recovery must detach all persisted states from counters reused by the next router.
    #[tokio::test]
    async fn restart_invalidates_remote_ids_for_active_and_terminal_operations() {
        let directory = std::env::temp_dir().join(format!("mcp-recovery-{}", Uuid::new_v4()));
        let registry = OperationRegistry::in_directory(directory.clone())
            .await
            .unwrap();
        let owner = Uuid::new_v4();
        let mut ids = Vec::new();
        for state in [
            "running",
            "completed",
            "failed",
            "canceled",
            "interrupted",
            "unknown",
        ] {
            let record = registry.create(owner, vec![], "copy").await.unwrap();
            let mut current = record.lock().await;
            current.remote_id = Some(1);
            current.state = state.to_string();
            ids.push((current.id, state));
            drop(current);
            registry.update(&record).await.unwrap();
        }
        let recovered = OperationRegistry::in_directory(directory.clone())
            .await
            .unwrap();
        for (id, state) in ids {
            let record = recovered.get(id).await.unwrap().unwrap();
            let current = record.lock().await;
            assert_eq!(
                current.remote_id, None,
                "no recovered record may target a new router counter"
            );
            assert_eq!(
                current.state,
                if state == "running" {
                    "interrupted"
                } else {
                    state
                },
                "recovery preserves terminal outcomes and marks only unacknowledged active work interrupted"
            );
        }
        redoor_core::safe_fs::safe_rm_all(&directory).await.unwrap();
    }
}
