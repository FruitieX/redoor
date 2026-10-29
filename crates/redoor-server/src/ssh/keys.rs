//! On-disk layout of redoor-managed SSH keys and pinned host keys.
//!
//! ```text
//! <data_dir>/ssh/<device-id>/
//!     known_hosts           0600, pinned host key for this device only
//!     id_ed25519            0600, current OpenSSH private key (unencrypted in phase 1)
//!     id_ed25519.next       0600, key staged during setup or rotation
//! ```
//!
//! Everything is keyed by the permanent device id, so renames and reused names can never
//! orphan a key or hand it to another device. The pinned host key belongs to the device,
//! not to a key, so it survives rotation.
//!
//! The current key always has the same path, so switching to a new key is a single atomic
//! `rename(id_ed25519.next, id_ed25519)`: a concurrent agent start reads either the old or
//! the new complete key, and rotation needs no config write (which would stop the agent).
//! There is no `.pub` file, because a second file could not be switched in the same
//! rename; the public key is derived from the private key whenever it is loaded.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use redoor_protocol::ids::AgentId;
use ssh_key::{HashAlg, LineEnding, PrivateKey, PublicKey, private::Ed25519Keypair};
use tokio::io::AsyncWriteExt;
use zeroize::Zeroizing;

/// File name of the device's current private key.
const PRIVATE_KEY_FILE: &str = "id_ed25519";
/// File name of a private key that is installed and verified but not yet current.
const STAGED_KEY_FILE: &str = "id_ed25519.next";
/// File name of the pinned host key inside a device directory.
const KNOWN_HOSTS_FILE: &str = "known_hosts";

/// Owns `<data_dir>/ssh` and the file modes of everything under it.
#[derive(Clone, Debug)]
pub(crate) struct SshKeyStore {
    /// `<data_dir>/ssh`.
    root: PathBuf,
    /// Serializes `authorized_keys` rewrites across all devices of this server. Several
    /// devices can share one remote account, and the per-device busy set does not stop two
    /// of them from reading and replacing the same file at once and losing a line.
    authorized_keys_lock: Arc<tokio::sync::Mutex<()>>,
}

impl SshKeyStore {
    /// Stores keys under `<data_dir>/ssh`.
    pub(crate) fn for_data_dir(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("ssh"),
            authorized_keys_lock: Arc::default(),
        }
    }

    /// Held while one `authorized_keys` rewrite runs on a device.
    pub(crate) async fn lock_authorized_keys(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.authorized_keys_lock.lock().await
    }

    /// Directory holding everything that belongs to one device.
    fn device_dir(&self, device: &AgentId) -> PathBuf {
        self.root.join(device.as_str())
    }

    /// The device's current private key, the one every agent start uses.
    fn current_key_path(&self, device: &AgentId) -> PathBuf {
        self.device_dir(device).join(PRIVATE_KEY_FILE)
    }

    /// The key staged by an unfinished setup or rotation.
    fn staged_key_path(&self, device: &AgentId) -> PathBuf {
        self.device_dir(device).join(STAGED_KEY_FILE)
    }

    /// The device's pinned known_hosts file.
    pub(crate) fn known_hosts_path(&self, device: &AgentId) -> PathBuf {
        self.device_dir(device).join(KNOWN_HOSTS_FILE)
    }

    /// Creates the device directory (and its parents) as 0700, so key material is never
    /// reachable by other users even for a moment.
    async fn ensure_device_dir(&self, device: &AgentId) -> std::io::Result<PathBuf> {
        let dir = self.device_dir(device);
        create_private_dir_all(&dir).await?;
        Ok(dir)
    }

    /// Generates a new ed25519 key in redoor's memory and writes it as the staged key.
    ///
    /// Generating in-process instead of running `ssh-keygen` means the private key is
    /// written only by redoor, directly with 0600, and never passes through another process.
    /// A staged key left behind by a crashed setup or rotation is simply replaced: it was
    /// never current, so nothing refers to it.
    pub(crate) async fn stage(&self, device: &AgentId) -> std::io::Result<ManagedKey> {
        let mut seed = Zeroizing::new([0u8; 32]);
        getrandom::fill(seed.as_mut()).map_err(|error| {
            std::io::Error::other(format!("the OS random source is unavailable: {error}"))
        })?;
        let mut private_key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
        // The comment only helps someone inspecting the file; the fingerprint does not
        // depend on it.
        let fingerprint = private_key.public_key().fingerprint(HashAlg::Sha256);
        private_key.set_comment(marker(device, &fingerprint.to_string()));
        let private_text = private_key
            .to_openssh(LineEnding::LF)
            .map_err(std::io::Error::other)?;

        self.ensure_device_dir(device).await?;
        let path = self.staged_key_path(device);
        write_atomically(&path, private_text.as_bytes(), 0o600).await?;
        Ok(ManagedKey {
            device: device.clone(),
            private_key_path: path,
            public_key: private_key.public_key().clone(),
        })
    }

    /// Loads the device's current key, failing when it is missing or unreadable so a broken
    /// key is reported instead of handed to ssh.
    pub(crate) async fn load(&self, device: &AgentId) -> std::io::Result<ManagedKey> {
        let path = self.current_key_path(device);
        // One read of the whole file: a rotation renaming a new key into place meanwhile
        // yields either the old or the new key, never a mix.
        let text = Zeroizing::new(tokio::fs::read(&path).await?);
        let private_key = PrivateKey::from_openssh(text.as_slice()).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?;
        Ok(ManagedKey {
            device: device.clone(),
            private_key_path: path,
            public_key: private_key.public_key().clone(),
        })
    }

    /// Makes the staged key the current one with a single atomic rename, so every reader
    /// sees either the old or the new complete key, and returns it at its new path.
    ///
    /// The directory is synced afterwards because the caller may persist a config that
    /// refers to the key next; after a power loss that config must not survive without it.
    pub(crate) async fn commit_staged(&self, staged: &ManagedKey) -> std::io::Result<ManagedKey> {
        let device = &staged.device;
        let current = self.current_key_path(device);
        tokio::fs::rename(self.staged_key_path(device), &current).await?;
        sync_dir(&self.device_dir(device)).await?;
        Ok(ManagedKey {
            private_key_path: current,
            ..staged.clone()
        })
    }

    /// Deletes the staged key after a failed setup or rotation, so no private key is left
    /// that is installed nowhere.
    pub(crate) async fn delete_staged(&self, device: &AgentId) -> std::io::Result<()> {
        remove_file_if_present(&self.staged_key_path(device)).await
    }

    /// Deletes the current and staged keys of a device (and temp files a crash may have
    /// left while writing one) but keeps its pinned host key, because the machine has not
    /// changed and setting up a key again should not ask for confirmation.
    pub(crate) async fn delete_keys(&self, device: &AgentId) -> std::io::Result<()> {
        let mut entries = match tokio::fs::read_dir(self.device_dir(device)).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            // Only key files; the pinned known_hosts and probe files stay.
            if name.to_str().is_some_and(is_key_file_name) {
                remove_file_if_present(&entry.path()).await?;
            }
        }
        Ok(())
    }

    /// Deletes everything stored for a device, including the pinned host key, for a deleted
    /// device or a target that may now be another machine.
    pub(crate) async fn delete_device(&self, device: &AgentId) -> std::io::Result<()> {
        remove_dir_if_present(&self.device_dir(device)).await
    }

    /// Reads the pinned host key, or `None` when the device has none yet.
    pub(crate) async fn pinned_host_key(
        &self,
        device: &AgentId,
    ) -> std::io::Result<Option<HostKey>> {
        match tokio::fs::read_to_string(self.known_hosts_path(device)).await {
            // An existing but damaged pin must fail closed, including after key removal;
            // treating it as absent would silently restore trust-on-first-use credentials.
            Ok(content) => HostKey::from_known_hosts(&content)
                .map(Some)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("The pinned SSH host key for device '{device}' is invalid"),
                    )
                }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Replaces the device's pinned host key with `key`, recorded under the device id.
    pub(crate) async fn pin_host_key(
        &self,
        device: &AgentId,
        key: &HostKey,
    ) -> std::io::Result<()> {
        self.ensure_device_dir(device).await?;
        write_atomically(
            &self.known_hosts_path(device),
            format!("{}\n", key.known_hosts_line(device.as_str())?).as_bytes(),
            0o600,
        )
        .await
    }

    /// A fresh, not yet existing path in the device directory for one host key probe.
    /// Unique per call so concurrent probes of one device cannot read each other's result.
    pub(crate) async fn probe_known_hosts_path(
        &self,
        device: &AgentId,
    ) -> std::io::Result<PathBuf> {
        let dir = self.ensure_device_dir(device).await?;
        Ok(dir.join(format!(".probe-{}", uuid::Uuid::new_v4())))
    }
}

/// The `authorized_keys` comment that ties a line to one key of one device. The key's
/// `SHA256:` fingerprint names it, so the marker is readable and unique without a stored
/// key id; matching only [`device_marker`] finds every line the device ever installed.
pub(crate) fn marker(device: &AgentId, fingerprint: &str) -> String {
    format!("{}{fingerprint}", device_marker(device))
}

/// The marker prefix shared by every `authorized_keys` line of one device.
pub(crate) fn device_marker(device: &AgentId) -> String {
    format!("redoor:{device}:")
}

/// One managed key on disk, current or staged.
#[derive(Clone, Debug)]
pub(crate) struct ManagedKey {
    /// Device the key belongs to.
    device: AgentId,
    /// Private key file handed to ssh as `IdentityFile`.
    pub(crate) private_key_path: PathBuf,
    /// Public half, derived from the private key and installed on the device.
    public_key: PublicKey,
}

impl ManagedKey {
    /// The line installed in the device's `authorized_keys`.
    ///
    /// `restrict` turns off PTY allocation, agent and X11 forwarding and `~/.ssh/rc`, none of
    /// which redoor uses; `port-forwarding` puts back the `-R` tunnel the agent needs.
    pub(crate) fn authorized_keys_line(&self) -> std::io::Result<String> {
        let mut public_key = self.public_key.clone();
        public_key.set_comment("");
        let key = public_key.to_openssh().map_err(std::io::Error::other)?;
        Ok(format!(
            "restrict,port-forwarding {} {}",
            key.trim(),
            marker(&self.device, &self.fingerprint())
        ))
    }

    /// `SHA256:` fingerprint of the public key, as sshd logs it. Also the key's identity in
    /// the `authorized_keys` marker and keep list.
    pub(crate) fn fingerprint(&self) -> String {
        self.public_key.fingerprint(HashAlg::Sha256).to_string()
    }
}

/// A host public key read from a known_hosts file.
#[derive(Clone, Debug)]
pub(crate) struct HostKey {
    /// The key itself; host names and markers are not kept.
    public_key: PublicKey,
}

impl HostKey {
    /// Reads the first plain host key entry. Comments, `@cert-authority` and `@revoked`
    /// lines are skipped, and the host field is ignored so hashed names work too.
    pub(crate) fn from_known_hosts(content: &str) -> Option<Self> {
        content.lines().find_map(Self::from_known_hosts_line)
    }

    /// Parses one `host keytype base64 [comment]` line.
    pub(crate) fn from_known_hosts_line(line: &str) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('@') {
            return None;
        }
        let mut fields = line.split_whitespace();
        let _host = fields.next()?;
        let key_type = fields.next()?;
        let key = fields.next()?;
        let public_key = PublicKey::from_openssh(&format!("{key_type} {key}")).ok()?;
        Some(Self { public_key })
    }

    /// `SHA256:` fingerprint, the format `ssh-keygen -l` and OpenSSH prompts show.
    pub(crate) fn fingerprint(&self) -> String {
        self.public_key.fingerprint(HashAlg::Sha256).to_string()
    }

    /// OpenSSH key type, for example `ssh-ed25519`.
    pub(crate) fn key_type(&self) -> String {
        self.public_key.algorithm().as_str().to_string()
    }

    /// Whether both are the same key, ignoring comments.
    pub(crate) fn same_key(&self, other: &HostKey) -> bool {
        self.public_key.key_data() == other.public_key.key_data()
    }

    /// Whether a fingerprint typed or confirmed by the user is this key's. Accepts the value
    /// with or without the `SHA256:` prefix, because that is how users copy it.
    pub(crate) fn matches_fingerprint(&self, fingerprint: &str) -> bool {
        let fingerprint = fingerprint.trim();
        let own = self.fingerprint();
        fingerprint == own || own.strip_prefix("SHA256:") == Some(fingerprint)
    }

    /// The known_hosts line pinning this key under `alias`.
    fn known_hosts_line(&self, alias: &str) -> std::io::Result<String> {
        let mut public_key = self.public_key.clone();
        public_key.set_comment("");
        let key = public_key.to_openssh().map_err(std::io::Error::other)?;
        Ok(format!("{alias} {}", key.trim()))
    }
}

/// Creates `dir` and missing parents with mode 0700.
async fn create_private_dir_all(dir: &Path) -> std::io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(dir).await
}

/// Whether a device directory entry holds private key material: the current or staged key,
/// or the temp file [`write_atomically`] uses while writing one.
fn is_key_file_name(name: &str) -> bool {
    name == PRIVATE_KEY_FILE
        || name == STAGED_KEY_FILE
        || (name.starts_with(&format!(".{PRIVATE_KEY_FILE}.")) && name.ends_with(".tmp"))
}

/// Flushes a directory's entries, so a rename in it is durable before anything that
/// depends on it is written.
async fn sync_dir(dir: &Path) -> std::io::Result<()> {
    tokio::fs::File::open(dir).await?.sync_all().await
}

/// Writes `content` to a sibling temp file created with its final `mode`, then renames it
/// over `path`, so the file is never briefly readable with a wider mode or half-written.
async fn write_atomically(path: &Path, content: &[u8], mode: u32) -> std::io::Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other(format!("'{}' has no file name", path.display())))?;
    let temp_path = path.with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(mode);
    let result = async {
        let mut file = options.open(&temp_path).await?;
        file.write_all(content).await?;
        file.sync_all().await?;
        tokio::fs::rename(&temp_path, path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp_path).await;
    }
    result
}

/// Deletes one file; a missing file is already deleted.
async fn remove_file_if_present(path: &Path) -> std::io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Recursively deletes `dir` with `safe_rm_all`; a missing directory is already deleted.
async fn remove_dir_if_present(dir: &Path) -> std::io::Result<()> {
    match tokio::fs::symlink_metadata(dir).await {
        Ok(_) => redoor_core::safe_fs::safe_rm_all(dir).await,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Mode bits of a path, for asserting the store never widens them.
    async fn mode(path: &Path) -> u32 {
        tokio::fs::metadata(path)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    /// Names in a directory, sorted, for asserting exactly which files exist.
    async fn file_names(dir: &Path) -> Vec<String> {
        let mut entries = tokio::fs::read_dir(dir).await.unwrap();
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        names
    }

    /// Keys and their directory get their final modes, so no other user can read the
    /// private key even while it is being written, and only the private key is stored.
    #[tokio::test]
    async fn committed_key_is_the_only_private_file() {
        let home = redoor_core::test_support::TempDir::create();
        let store = SshKeyStore::for_data_dir(home.path());
        let device = AgentId::generate();
        let staged = store.stage(&device).await.unwrap();
        let dir = store.device_dir(&device);

        // A staged key waits next to the current one under its own name.
        assert_eq!(file_names(&dir).await, vec![STAGED_KEY_FILE]);
        // The staged key is as private as the current one, because it is a working key.
        assert_eq!(mode(&staged.private_key_path).await, 0o600);

        let key = store.commit_staged(&staged).await.unwrap();
        // The commit moved the staged key into place; no `.pub`, temp or staged file is left.
        assert_eq!(file_names(&dir).await, vec![PRIVATE_KEY_FILE]);
        // The private key is readable only by the server's user.
        assert_eq!(mode(&key.private_key_path).await, 0o600);
        // The device directory cannot be listed by others.
        assert_eq!(mode(&dir).await, 0o700);
    }

    /// A loaded key is the committed one, its public half is derived from the private key,
    /// and its authorized_keys line carries the marker used for rotation and removal.
    #[tokio::test]
    async fn loaded_key_matches_committed_key() {
        let home = redoor_core::test_support::TempDir::create();
        let store = SshKeyStore::for_data_dir(home.path());
        let device = AgentId::generate();
        let key = store
            .commit_staged(&store.stage(&device).await.unwrap())
            .await
            .unwrap();
        let loaded = store.load(&device).await.unwrap();

        // The same public key comes back from the private key file alone.
        assert_eq!(loaded.fingerprint(), key.fingerprint());
        let line = loaded.authorized_keys_line().unwrap();
        // The key is restricted except for the reverse tunnel.
        assert!(
            line.starts_with("restrict,port-forwarding ssh-ed25519 "),
            "{line}"
        );
        // The marker names the device and the key's fingerprint.
        assert!(
            line.ends_with(&format!(" redoor:{device}:{}", key.fingerprint())),
            "{line}"
        );
        // The fingerprint is the `ssh-keygen -l` format the user sees in the UI.
        assert!(key.fingerprint().starts_with("SHA256:"));
    }

    /// Rotation replaces the current key in one rename, and a stale staged key from an
    /// interrupted run is overwritten instead of blocking the next attempt.
    #[tokio::test]
    async fn rotation_replaces_the_current_key_and_stale_staged_keys() {
        let home = redoor_core::test_support::TempDir::create();
        let store = SshKeyStore::for_data_dir(home.path());
        let device = AgentId::generate();
        let old = store
            .commit_staged(&store.stage(&device).await.unwrap())
            .await
            .unwrap();
        let stale = store.stage(&device).await.unwrap();
        let new = store.stage(&device).await.unwrap();

        // Staging again replaced the stale staged key.
        assert_ne!(stale.fingerprint(), new.fingerprint());
        // Until the commit, every start still loads the old key.
        assert_eq!(
            store.load(&device).await.unwrap().fingerprint(),
            old.fingerprint()
        );
        store.commit_staged(&new).await.unwrap();
        // After the commit the new key is current ...
        assert_eq!(
            store.load(&device).await.unwrap().fingerprint(),
            new.fingerprint()
        );
        // ... and nothing else is left of the old or the stale key.
        assert_eq!(
            file_names(&store.device_dir(&device)).await,
            vec![PRIVATE_KEY_FILE]
        );
        // A failed attempt's staged key can be deleted without touching the current one.
        store.stage(&device).await.unwrap();
        store.delete_staged(&device).await.unwrap();
        assert_eq!(
            file_names(&store.device_dir(&device)).await,
            vec![PRIVATE_KEY_FILE]
        );
    }

    /// The pinned host key round-trips under the device id and deleting keys keeps it.
    #[tokio::test]
    async fn pin_survives_key_deletion() {
        let home = redoor_core::test_support::TempDir::create();
        let store = SshKeyStore::for_data_dir(home.path());
        let device = AgentId::generate();
        let host_key = HostKey::from_known_hosts(
            "# comment\n@revoked * ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl\n|1|hashed= ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl\n",
        )
        .expect("the hashed entry is read, the revoked one skipped");
        store.pin_host_key(&device, &host_key).await.unwrap();
        let staged = store.stage(&device).await.unwrap();
        store.commit_staged(&staged).await.unwrap();
        store.stage(&device).await.unwrap();
        store.delete_keys(&device).await.unwrap();

        // The current and the staged key are gone; only the pin is left.
        assert_eq!(
            file_names(&store.device_dir(&device)).await,
            vec![KNOWN_HOSTS_FILE]
        );
        assert!(store.load(&device).await.is_err());
        let pinned = store.pinned_host_key(&device).await.unwrap().unwrap();
        // The pin is still there, so setting up a key again needs no confirmation.
        assert!(pinned.same_key(&host_key));
        // The pinned file is private and names the device id, which is the HostKeyAlias.
        assert_eq!(mode(&store.known_hosts_path(&device)).await, 0o600);
        let content = tokio::fs::read_to_string(store.known_hosts_path(&device))
            .await
            .unwrap();
        assert!(
            content.starts_with(&format!("{device} ssh-ed25519 ")),
            "{content}"
        );
        // Confirmations with or without the prefix match.
        let fingerprint = host_key.fingerprint();
        assert!(pinned.matches_fingerprint(&fingerprint));
        assert!(pinned.matches_fingerprint(fingerprint.trim_start_matches("SHA256:")));
        assert!(!pinned.matches_fingerprint("SHA256:something-else"));

        store.delete_device(&device).await.unwrap();
        // Deleting the device removes the pin as well.
        assert!(store.pinned_host_key(&device).await.unwrap().is_none());
    }

    /// Losing the contents of a pin must not make password or default-key logins trust a
    /// new host automatically.
    #[tokio::test]
    async fn invalid_pin_is_an_error_instead_of_missing_trust() {
        let home = redoor_core::test_support::TempDir::create();
        let store = SshKeyStore::for_data_dir(home.path());
        let device = AgentId::generate();
        store.ensure_device_dir(&device).await.unwrap();
        tokio::fs::write(store.known_hosts_path(&device), "not a host key")
            .await
            .unwrap();
        let error = store.pinned_host_key(&device).await.unwrap_err();
        // Only a nonexistent pin may select the initial trust-on-first-use policy.
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
}
