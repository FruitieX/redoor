//! The device id a standalone agent remembers in its data directory.
//!
//! Managed agents and relays get their id from the server or relay config with `--agent-id`,
//! because several managed entries can share one remote data dir and the server needs the id
//! before the first connection. A standalone agent has no config entry on the server, so its
//! own data dir is the only place that can remember the id across restarts.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use redoor_core::atomic_rename::{AtomicRenameOutcome, rename_without_replacement};
use redoor_core::log;
use redoor_protocol::{ids::AgentId, wire::log::Level};
use tokio::io::AsyncWriteExt;

/// File name of the id inside the agent's data directory.
const ID_FILE_NAME: &str = "agent-id";

/// Where an agent's device id came from, which also tells how the agent must react when the
/// server says another connection uses the same id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DeviceIdSource {
    /// `--agent-id` from the server's `[[agents]]` entry or a relay. The server or relay
    /// supervises this process, so being replaced means it is a stale duplicate of itself.
    Assigned,
    /// The standalone agent's own id file. The agent holds the single-instance lock of its
    /// data directory, so another connection with this id comes from a machine that got a
    /// copy of this file.
    File(PathBuf),
}

/// The `<data_dir>/agent-id` file of a standalone agent.
pub(crate) struct DeviceIdFile {
    /// Absolute path of the id file.
    path: PathBuf,
}

impl DeviceIdFile {
    /// Points at the id file inside the agent's data directory.
    pub(crate) fn in_data_dir(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(ID_FILE_NAME),
        }
    }

    /// Absolute path of the id file, shown to the operator when a copy of it is in use on
    /// another machine.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Reuses the stored id, or creates and stores a new one on first start.
    ///
    /// Deleting the file is the documented way to give a cloned machine its own identity, so
    /// a missing file is normal; a file with something else in it is an error, because
    /// silently replacing it would turn the device into a new one.
    pub(crate) async fn load_or_create(&self) -> Result<AgentId> {
        if let Some(id) = self.load().await? {
            return Ok(id);
        }
        let id = AgentId::generate();
        match self.create(&id).await? {
            AtomicRenameOutcome::Renamed => {
                log!(
                    Level::Info,
                    "Created a new device id: agent_id={}, path={}",
                    id,
                    self.path.display()
                );
                Ok(id)
            }
            // Another agent with the same data dir won the race; both must use its id.
            AtomicRenameOutcome::DestinationExists => self
                .load()
                .await?
                .with_context(|| format!("Device id file '{}' vanished", self.path.display())),
            outcome => bail!(
                "Failed to store device id file '{}': {outcome:?}",
                self.path.display()
            ),
        }
    }

    /// Reads the stored id; `None` when the file does not exist yet.
    async fn load(&self) -> Result<Option<AgentId>> {
        let content = match tokio::fs::read_to_string(&self.path).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Failed to read device id file '{}'", self.path.display())
                });
            }
        };
        let id = AgentId::parse(content.trim()).with_context(|| {
            format!(
                "Device id file '{}' is corrupt; delete it to give this agent a new identity",
                self.path.display()
            )
        })?;
        Ok(Some(id))
    }

    /// Writes the id to a private temporary file and publishes it without replacing an id
    /// another process stored meanwhile, so a crash can never leave a half-written file.
    async fn create(&self, id: &AgentId) -> Result<AtomicRenameOutcome> {
        let parent = self
            .path
            .parent()
            .with_context(|| format!("Device id path '{}' has no parent", self.path.display()))?;
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("Failed to create data directory '{}'", parent.display()))?;
        let temp_path = parent.join(format!(
            ".{ID_FILE_NAME}.{}.{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let result = async {
            let mut file = options.open(&temp_path).await?;
            file.write_all(format!("{id}\n").as_bytes()).await?;
            file.sync_all().await?;
            rename_without_replacement(&temp_path, &self.path).await
        }
        .await;
        if !matches!(result, Ok(AtomicRenameOutcome::Renamed)) {
            let _ = tokio::fs::remove_file(&temp_path).await;
        }
        result.with_context(|| format!("Failed to store device id file '{}'", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A standalone agent must keep its identity across restarts and get a new one only when
    /// the file is deleted.
    #[tokio::test]
    async fn id_is_stable_until_the_file_is_deleted() {
        let directory = redoor_core::test_support::TempDir::create();
        let file = DeviceIdFile::in_data_dir(&directory.path().join("data"));
        let first = file.load_or_create().await.unwrap();
        // The second start reads the stored id instead of creating another one.
        assert_eq!(file.load_or_create().await.unwrap(), first);
        tokio::fs::remove_file(directory.path().join("data/agent-id"))
            .await
            .unwrap();
        // Deleting the file is how a cloned machine gets its own identity.
        assert_ne!(file.load_or_create().await.unwrap(), first);
    }

    /// A damaged file must not be replaced silently, since that would turn the device into a
    /// new one and break token scopes.
    #[tokio::test]
    async fn corrupt_file_is_an_error() {
        let directory = redoor_core::test_support::TempDir::create();
        tokio::fs::write(directory.path().join("agent-id"), "pi\n")
            .await
            .unwrap();
        let error = DeviceIdFile::in_data_dir(directory.path())
            .load_or_create()
            .await
            .unwrap_err();
        // The message tells the operator how to recover.
        assert!(format!("{error:#}").contains("delete it"), "{error:#}");
    }
}
