//! Durable partial storage with bounded reads and inode-checked ownership.

use crate::staging::StagingKind;
use redoor_protocol::wire::partials::{PartialManifest, PartialRecord, SignedPartial};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

/// Whole segments keep recovery deterministic after torn writes.
pub const SEGMENT_SIZE: u64 = 64 * 1024 * 1024;
/// Wall-clock timestamps let cleanup survive process and machine restarts.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Small manifest directory replaces any persistent server transfer database.
#[derive(Clone, Debug)]
pub struct PartialStore {
    directory: PathBuf,
}

impl PartialStore {
    /// Uses the application namespace so dev and production cannot share resume state.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            directory: data_dir.join("partials"),
        }
    }

    /// Full path hashes avoid ambiguous or unsafe caller-supplied manifest names.
    pub fn key(destination: &str) -> String {
        blake3::hash(destination.as_bytes()).to_hex().to_string()
    }

    /// Restricts API keys to the generated filename format before joining paths.
    fn manifest_path(&self, key: &str) -> std::io::Result<PathBuf> {
        if key.len() != 64
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(std::io::Error::other("Invalid partial key"));
        }
        Ok(self.directory.join(format!("{key}.json")))
    }

    /// Atomic replacement and directory fsync make manifests durable before payload starts.
    pub async fn write(&self, manifest: &PartialManifest) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.directory).await?;
        let path = self.manifest_path(&Self::key(&manifest.signed.authorization.dest_path))?;
        let temp = self.directory.join(format!(".{}", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .await?;
        let result = async {
            file.write_all(&serde_json::to_vec(manifest)?).await?;
            file.sync_all().await?;
            tokio::fs::rename(&temp, path).await?;
            File::open(&self.directory).await?.sync_all().await
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(temp).await;
        }
        result
    }

    /// Bounded, no-follow reads refuse untrusted large or redirected metadata.
    pub async fn read(&self, key: &str) -> std::io::Result<PartialManifest> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.manifest_path(key)?)
            .await?;
        let metadata = file.metadata().await?;
        if !metadata.is_file() || metadata.len() > 64 * 1024 {
            return Err(std::io::Error::other("Invalid partial manifest"));
        }
        let mut bytes = Vec::new();
        file.take(64 * 1024 + 1).read_to_end(&mut bytes).await?;
        let manifest: PartialManifest = serde_json::from_slice(&bytes)?;
        if Self::key(&manifest.signed.authorization.dest_path) != key {
            return Err(std::io::Error::other("Partial key mismatch"));
        }
        Ok(manifest)
    }

    /// Only agent-owned, single-link regular files may be resumed or deleted.
    pub async fn open(manifest: &PartialManifest) -> std::io::Result<File> {
        let dest = Path::new(&manifest.signed.authorization.dest_path);
        let partial = Path::new(&manifest.partial_path);
        let expected = StagingKind::partial_of(dest);
        let fallback_prefix = format!(
            ".redoor-partial-{}-",
            &Self::key(&manifest.signed.authorization.dest_path)[..16]
        );
        if partial != expected
            && !(partial.parent() == expected.parent()
                && partial
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&fallback_prefix)))
        {
            return Err(std::io::Error::other("Invalid partial path"));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(partial)
            .await?;
        let metadata = file.metadata().await?;
        if !metadata.is_file()
            || metadata.uid() != nix::unistd::Uid::effective().as_raw()
            || metadata.nlink() != 1
            || metadata.dev() != manifest.partial_device
            || metadata.ino() != manifest.partial_inode
        {
            return Err(std::io::Error::other("Partial file identity mismatch"));
        }
        Ok(file)
    }

    /// Missing or invalid partials are not candidates; unsafe entries are left untouched.
    pub async fn probe(&self, key: &str) -> std::io::Result<Option<PartialRecord>> {
        self.manifest_path(key)?;
        let Ok(manifest) = self.read(key).await else {
            return Ok(None);
        };
        let Ok(file) = Self::open(&manifest).await else {
            return Ok(None);
        };
        let partial_bytes = file.metadata().await?.len();
        Ok(Some(PartialRecord {
            key: key.to_string(),
            manifest,
            partial_bytes,
        }))
    }

    /// Enumerates only the small metadata directory, never destination trees.
    pub async fn list(&self) -> std::io::Result<Vec<PartialRecord>> {
        let mut entries = match tokio::fs::read_dir(&self.directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut records = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let Some(key) = entry
                .path()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            if let Ok(Some(record)) = self.probe(&key).await {
                records.push(record);
            }
        }
        Ok(records)
    }

    /// Pause must persist both file contents and its directory entry before acknowledging.
    pub async fn persist(&self, key: &str) -> std::io::Result<()> {
        let mut manifest = self.read(key).await?;
        let file = Self::open(&manifest).await?;
        file.sync_all().await?;
        let parent = Path::new(&manifest.partial_path)
            .parent()
            .ok_or_else(|| std::io::Error::other("Missing partial parent"))?;
        File::open(parent).await?.sync_all().await?;
        manifest.updated_at = now();
        self.write(&manifest).await
    }

    /// Removes only a still-owned inode, leaving replacement entries alone.
    pub async fn discard(&self, key: &str) -> std::io::Result<()> {
        if let Ok(manifest) = self.read(key).await
            && Self::open(&manifest).await.is_ok()
        {
            let metadata = tokio::fs::symlink_metadata(&manifest.partial_path).await?;
            if metadata.dev() == manifest.partial_device
                && metadata.ino() == manifest.partial_inode
                && metadata.is_file()
            {
                tokio::fs::remove_file(&manifest.partial_path).await?;
            }
        }
        self.forget(key).await
    }

    /// Successful publication consumes the partial but still needs to remove its metadata.
    pub async fn forget(&self, key: &str) -> std::io::Result<()> {
        match tokio::fs::remove_file(self.manifest_path(key)?).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }

    /// New transfers replace owned stale entries and avoid claiming unsafe collisions.
    pub async fn create(&self, signed: SignedPartial) -> std::io::Result<(PartialManifest, File)> {
        let key = Self::key(&signed.authorization.dest_path);
        let had_manifest = tokio::fs::symlink_metadata(self.manifest_path(&key)?)
            .await
            .is_ok();
        self.discard(&key).await?;
        let expected = StagingKind::partial_of(Path::new(&signed.authorization.dest_path));
        let mut path = expected.clone();
        let file = loop {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .await
            {
                Ok(file) => break file,
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists && path == expected =>
                {
                    // Recover a lost manifest only for an agent-owned orphan, preserving replacements.
                    if !had_manifest {
                        let orphan = OpenOptions::new()
                            .read(true)
                            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                            .open(&expected)
                            .await;
                        if let Ok(orphan) = orphan {
                            let metadata = orphan.metadata().await?;
                            let current = tokio::fs::symlink_metadata(&expected).await?;
                            if metadata.is_file()
                                && metadata.uid() == nix::unistd::Uid::effective().as_raw()
                                && metadata.nlink() == 1
                                && current.is_file()
                                && current.dev() == metadata.dev()
                                && current.ino() == metadata.ino()
                            {
                                tokio::fs::remove_file(&expected).await?;
                                continue;
                            }
                        }
                    }
                    // Unsafe collisions stay untouched; the fallback also fits NAME_MAX.
                    path = expected.with_file_name(format!(
                        ".redoor-partial-{}-{}",
                        &key[..16],
                        uuid::Uuid::new_v4()
                    ));
                }
                Err(error) => return Err(error),
            }
        };
        let metadata = file.metadata().await?;
        let manifest = PartialManifest {
            signed,
            partial_path: path.to_string_lossy().into_owned(),
            partial_device: metadata.dev(),
            partial_inode: metadata.ino(),
            updated_at: now(),
        };
        let durability = async {
            file.sync_all().await?;
            let parent = path
                .parent()
                .ok_or_else(|| std::io::Error::other("Missing partial parent"))?;
            File::open(parent).await?.sync_all().await?;
            self.write(&manifest).await
        }
        .await;
        if let Err(error) = durability {
            let _ = tokio::fs::remove_file(path).await;
            return Err(error);
        }
        Ok((manifest, file))
    }

    /// Active destinations are excluded so a long copy cannot be expired underneath its worker.
    ///
    /// Only an unreadable manifest directory is an error; failures of single entries are
    /// logged and skipped.
    pub async fn cleanup(
        &self,
        age_seconds: u64,
        active: impl Fn(&str) -> bool,
    ) -> std::io::Result<()> {
        let mut entries = match tokio::fs::read_dir(&self.directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        while let Some(entry) = entries.next_entry().await? {
            let Some(key) = entry
                .path()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let Ok(manifest) = self.read(&key).await else {
                continue;
            };
            if active(&manifest.signed.authorization.dest_path) {
                continue;
            }
            let result = if now().saturating_sub(manifest.updated_at) > age_seconds {
                self.discard(&key).await
            } else if tokio::fs::symlink_metadata(&manifest.partial_path)
                .await
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                self.forget(&key).await
            } else {
                Ok(())
            };
            // One undeletable partial must not keep every other expired one on disk.
            if let Err(error) = result {
                crate::log!(
                    redoor_protocol::wire::log::Level::Warning,
                    "Partial cleanup skipped an entry: key={}, partial_path={}, error={}",
                    key,
                    manifest.partial_path,
                    error
                );
            }
        }
        Ok(())
    }
}

/// Sequential bounded hashing supports slow disks without buffering a whole segment.
pub async fn hash_segment(
    file: &mut File,
    start: u64,
    length: u64,
    cancel: &tokio_util::sync::CancellationToken,
) -> std::io::Result<String> {
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut buffer = vec![0; 8 * 1024 * 1024];
    let mut remaining = length;
    let mut hasher = blake3::Hasher::new();
    while remaining > 0 {
        if cancel.is_cancelled() {
            return Err(std::io::Error::other("Hashing canceled"));
        }
        let limit = remaining.min(buffer.len() as u64) as usize;
        let count = file.read(&mut buffer[..limit]).await?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Source changed while verifying",
            ));
        }
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// A fixed ordered tuple freezes the v1 signature encoding independently of JSON object order.
pub fn canonical(authorization: &redoor_protocol::wire::partials::PartialAuthorization) -> Vec<u8> {
    let a = authorization;
    serde_json::to_vec(&(
        a.version,
        &a.source_agent_id,
        &a.source_path,
        &a.source_identity,
        &a.dest_agent_id,
        &a.dest_path,
        a.operation,
        a.on_existing,
        a.created_at,
    ))
    .expect("authorization contains only serializable fields")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{partial_signatures::PartialSigner, test_support::TempDir};
    use redoor_protocol::{
        api::transfers::CopyExistingMode,
        ids::AgentId,
        wire::{
            command::MoveSourceIdentity,
            partials::{PartialAuthorization, PartialOperation},
        },
    };

    /// Uses a real signer so tests also cover the canonical authorization encoding.
    async fn signed(root: &Path, destination: &Path) -> SignedPartial {
        PartialSigner::new(root.join("server"))
            .sign(PartialAuthorization {
                version: 1,
                source_agent_id: AgentId::from("source"),
                source_path: "/source".into(),
                source_identity: MoveSourceIdentity {
                    device: 1,
                    inode: 2,
                    size: 100,
                    modified_seconds: 3,
                    modified_nanoseconds: 4,
                    is_directory: false,
                },
                dest_agent_id: AgentId::from("dest"),
                dest_path: destination.to_string_lossy().into_owned(),
                operation: PartialOperation::Copy,
                on_existing: CopyExistingMode::Error,
                created_at: 5,
            })
            .await
            .unwrap()
    }

    /// Long multibyte names remain valid filenames while destination hashes distinguish them.
    #[test]
    fn partial_names_are_stable_and_length_safe() {
        let destination = PathBuf::from("/tmp").join("ä".repeat(125));
        let partial = StagingKind::partial_of(&destination);
        // UTF-8 truncation must leave room for the complete deterministic suffix.
        assert!(partial.file_name().unwrap().as_encoded_bytes().len() <= 255);
        assert_eq!(partial, StagingKind::partial_of(&destination));
        assert_ne!(
            partial,
            StagingKind::partial_of(&PathBuf::from("/other").join("ä".repeat(125)))
        );
    }

    /// A symlink collision belongs to someone else and must never be followed or removed.
    #[tokio::test]
    async fn symlink_collision_is_preserved_and_fresh_output_is_discoverable() {
        let temp = TempDir::create();
        let dest = temp.path().join("dest");
        let victim = temp.path().join("victim");
        tokio::fs::write(&victim, b"untouched").await.unwrap();
        let collision = StagingKind::partial_of(&dest);
        tokio::fs::symlink(&victim, &collision).await.unwrap();
        let store = PartialStore::new(temp.path().join("agent"));
        let (manifest, _) = store
            .create(signed(temp.path(), &dest).await)
            .await
            .unwrap();
        // Unsafe deterministic collisions select owned fallback staging instead.
        assert_ne!(Path::new(&manifest.partial_path), collision);
        assert!(
            store
                .probe(&PartialStore::key(&dest.to_string_lossy()))
                .await
                .unwrap()
                .is_some()
        );
        store
            .discard(&PartialStore::key(&dest.to_string_lossy()))
            .await
            .unwrap();
        // Discard must never modify the symlink's target or claim the symlink itself.
        assert_eq!(tokio::fs::read(&victim).await.unwrap(), b"untouched");
        assert!(
            tokio::fs::symlink_metadata(collision)
                .await
                .unwrap()
                .is_symlink()
        );
    }

    /// An inode replacement cannot be resumed or deleted by stale cleanup metadata.
    #[tokio::test]
    async fn cleanup_leaves_replacement_inode() {
        let temp = TempDir::create();
        let dest = temp.path().join("dest");
        let store = PartialStore::new(temp.path().join("agent"));
        let (mut manifest, file) = store
            .create(signed(temp.path(), &dest).await)
            .await
            .unwrap();
        tokio::fs::rename(&manifest.partial_path, temp.path().join("old-inode"))
            .await
            .unwrap();
        tokio::fs::write(&manifest.partial_path, b"replacement")
            .await
            .unwrap();
        manifest.updated_at = 0;
        store.write(&manifest).await.unwrap();
        // Keeping the old inode alive ensures this exercises substitution, not inode reuse.
        assert!(PartialStore::open(&manifest).await.is_err());
        store.cleanup(1, |_| false).await.unwrap();
        assert_eq!(
            tokio::fs::read(&manifest.partial_path).await.unwrap(),
            b"replacement"
        );
        assert!(
            store
                .read(&PartialStore::key(&dest.to_string_lossy()))
                .await
                .is_err()
        );
        drop(file);
    }

    /// Durable manifests and keys work across independent instances, with no process resume state.
    #[tokio::test]
    async fn persistence_and_signature_tampering() {
        let temp = TempDir::create();
        let dest = temp.path().join("dest");
        let store = PartialStore::new(temp.path().join("agent"));
        let (mut manifest, mut file) = store
            .create(signed(temp.path(), &dest).await)
            .await
            .unwrap();
        file.write_all(b"partial").await.unwrap();
        file.sync_all().await.unwrap();
        let records = PartialStore::new(temp.path().join("agent"))
            .list()
            .await
            .unwrap();
        // Length comes from the file, including writes made after the manifest was created.
        assert_eq!(records[0].partial_bytes, 7);
        let signer = PartialSigner::new(temp.path().join("server"));
        assert!(signer.verify(&manifest.signed).await.unwrap());
        manifest.signed.authorization.source_path = "/private".into();
        // Altering authorization cannot turn resume into an arbitrary source read.
        assert!(!signer.verify(&manifest.signed).await.unwrap());
        assert_eq!(
            canonical(&records[0].manifest.signed.authorization),
            canonical(
                &serde_json::from_slice(
                    &serde_json::to_vec(&records[0].manifest.signed.authorization).unwrap()
                )
                .unwrap()
            )
        );
    }

    /// Cleanup expires only inactive owned output and removes missing-file metadata too.
    #[tokio::test]
    async fn expired_partials_and_missing_files_are_cleaned() {
        let temp = TempDir::create();
        let dest = temp.path().join("dest");
        let store = PartialStore::new(temp.path().join("agent"));
        let (mut manifest, _) = store
            .create(signed(temp.path(), &dest).await)
            .await
            .unwrap();
        manifest.updated_at = 0;
        store.write(&manifest).await.unwrap();
        store.cleanup(1, |_| true).await.unwrap();
        // A long active transfer must never lose staging during the daily sweep.
        assert!(PartialStore::open(&manifest).await.is_ok());
        store.cleanup(1, |_| false).await.unwrap();
        // Expiration consumes both metadata and its verified owned inode.
        assert!(
            tokio::fs::symlink_metadata(&manifest.partial_path)
                .await
                .is_err()
        );
        assert!(
            store
                .read(&PartialStore::key(&dest.to_string_lossy()))
                .await
                .is_err()
        );
        let (manifest, _) = store
            .create(signed(temp.path(), &dest).await)
            .await
            .unwrap();
        tokio::fs::remove_file(&manifest.partial_path)
            .await
            .unwrap();
        store.cleanup(u64::MAX, |_| false).await.unwrap();
        // A disappeared partial has no recoverable data regardless of its age.
        assert!(
            store
                .read(&PartialStore::key(&dest.to_string_lossy()))
                .await
                .is_err()
        );
    }

    /// Reinstalling an agent must reclaim its deterministic orphan without accumulating output.
    #[tokio::test]
    async fn owned_orphans_are_replaced() {
        let temp = TempDir::create();
        let dest = temp.path().join("dest");
        let partial = StagingKind::partial_of(&dest);
        tokio::fs::write(&partial, b"orphan").await.unwrap();
        let store = PartialStore::new(temp.path().join("agent"));
        let (manifest, file) = store
            .create(signed(temp.path(), &dest).await)
            .await
            .unwrap();
        // A safe orphan is replaced at its recognizable deterministic name.
        assert_eq!(Path::new(&manifest.partial_path), partial);
        assert_eq!(file.metadata().await.unwrap().len(), 0);
    }

    /// Signatures must stay stable even if struct field order is changed in a later refactor.
    #[tokio::test]
    async fn canonical_authorization_encoding_is_frozen() {
        let temp = TempDir::create();
        let authorization = signed(temp.path(), Path::new("/dest")).await.authorization;
        let expected = r#"[1,"source","/source",{"device":1,"inode":2,"size":100,"modified_seconds":3,"modified_nanoseconds":4,"is_directory":false},"dest","/dest","copy","error",5]"#;
        // This fixed encoding protects persisted signatures across releases.
        assert_eq!(canonical(&authorization), expected.as_bytes());
    }

    /// A hard-linked partial could expose unpublished output under another name.
    #[tokio::test]
    async fn hard_links_are_refused() {
        let temp = TempDir::create();
        let dest = temp.path().join("dest");
        let store = PartialStore::new(temp.path().join("agent"));
        let (manifest, _) = store
            .create(signed(temp.path(), &dest).await)
            .await
            .unwrap();
        tokio::fs::hard_link(&manifest.partial_path, temp.path().join("peer"))
            .await
            .unwrap();
        // Single-link ownership is required both for resume and deletion.
        assert!(PartialStore::open(&manifest).await.is_err());
        store
            .discard(&PartialStore::key(&dest.to_string_lossy()))
            .await
            .unwrap();
        assert!(tokio::fs::try_exists(&manifest.partial_path).await.unwrap());
    }
}
