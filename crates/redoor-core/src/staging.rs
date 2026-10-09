//! Hidden staging paths shared by uploads, local copies, file edits and override backups.
//!
//! Every partially written output lives under a recognizable hidden name until it is published,
//! so one naming scheme and one owner type replace the per-feature temp path helpers that
//! previously drifted apart (different fallbacks, cleanup order and error handling).

use crate::log;
use redoor_protocol::wire::log::Level;
use std::path::{Path, PathBuf};

/// Purpose of a hidden path; selects its recognizable name tag and fallback stem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagingKind {
    /// Raw file upload staged next to its destination.
    Upload,
    /// Tar upload extracted next to its destination directory.
    UploadDirectory,
    /// Same-agent file copy staged next to its destination.
    LocalCopy,
    /// Same-agent directory copy staged next to its destination.
    LocalCopyDirectory,
    /// Previous destination held aside while an override publishes the replacement.
    OverrideBackup,
    /// Anonymous file-edit body staged in the temporary directory and unlinked immediately.
    FileEdit,
}

impl StagingKind {
    /// Name tag that lets operators and tests recognize leftovers of this operation.
    fn tag(self) -> &'static str {
        match self {
            Self::Upload => "redoor-upload",
            Self::UploadDirectory => "redoor-upload-dir",
            Self::LocalCopy => "redoor-local-copy",
            Self::LocalCopyDirectory => "redoor-local-copy-dir",
            Self::OverrideBackup => "redoor-override-backup",
            Self::FileEdit => "redoor-edit",
        }
    }

    /// Stem used when the destination has no UTF-8 file name to embed.
    fn fallback_name(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::UploadDirectory => "upload-dir",
            Self::LocalCopy => "copy",
            Self::LocalCopyDirectory => "copy-dir",
            Self::OverrideBackup => "destination",
            Self::FileEdit => "edit",
        }
    }

    /// Builds `.{name}.{tag}-{random}` beside `destination` so a later rename stays on the
    /// destination filesystem; the path is not created.
    pub fn sibling_of(self, destination: &Path) -> PathBuf {
        let file_name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(self.fallback_name());
        let name = format!(".{file_name}.{}-{}", self.tag(), fastrand::u64(..));
        match destination.parent() {
            Some(parent) => parent.join(name),
            None => Path::new(".").join(name),
        }
    }

    /// Builds an unpredictable name under the platform temporary directory for staging that is
    /// not tied to a destination filesystem.
    pub fn in_temp_dir(self) -> PathBuf {
        std::env::temp_dir().join(format!(
            ".{}-{}-{}",
            self.tag(),
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }
}

/// Owns one staging entry and removes it unless publication explicitly keeps it.
///
/// Callers await [`StagingPath::discard`] on ordinary failure paths so cleanup finishes before
/// they report; `Drop` is only the fallback for futures that are aborted mid-flight.
#[derive(Debug)]
pub struct StagingPath {
    path: PathBuf,
    armed: bool,
}

impl StagingPath {
    /// Takes ownership of a path the caller is about to create (or has just created).
    pub fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    /// Reserves a fresh sibling name for `destination` without creating it.
    pub fn sibling(destination: &Path, kind: StagingKind) -> Self {
        Self::new(kind.sibling_of(destination))
    }

    /// Location of the owned entry.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Removes the entry (file, symlink or directory tree) and waits for completion.
    pub async fn discard(mut self) {
        self.armed = false;
        remove_staging_entry(&self.path).await;
    }

    /// Disarms cleanup after publication renamed or consumed the entry.
    pub fn keep(mut self) -> PathBuf {
        self.armed = false;
        std::mem::take(&mut self.path)
    }
}

impl Drop for StagingPath {
    /// Schedules removal when an owner is dropped without discarding, e.g. an aborted task.
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let path = std::mem::take(&mut self.path);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move { remove_staging_entry(&path).await });
            }
            Err(_) => log!(
                Level::Warning,
                "Staging path leaked without a runtime to remove it: path={}",
                path.display()
            ),
        }
    }
}

/// Removes one staging entry without following symlinks; missing entries are already clean.
async fn remove_staging_entry(path: &Path) {
    let result = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() => crate::safe_fs::safe_rm_all(path).await,
        Ok(_) => tokio::fs::remove_file(path).await,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        log!(
            Level::Warning,
            "Failed to remove staging path: path={}, error={}",
            path.display(),
            error
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    /// Yields until a spawned cleanup removes `path`, bounded so a regression fails instead of hanging.
    async fn wait_until_removed(path: &Path) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while tokio::fs::try_exists(path).await.unwrap() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropped staging path should be removed");
    }

    /// Verifies sibling names stay on the destination filesystem and keep their tag.
    #[test]
    fn sibling_names_stay_beside_the_destination_and_are_recognizable() {
        let path = StagingKind::LocalCopy.sibling_of(Path::new("/data/dest.bin"));

        // A sibling keeps the final rename on the destination filesystem.
        assert_eq!(path.parent(), Some(Path::new("/data")));
        // The hidden tagged prefix lets cleanup tests and operators identify leftovers.
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".dest.bin.redoor-local-copy-")
        );
    }

    /// Verifies awaited discard removes both files and directory trees.
    #[tokio::test]
    async fn discard_removes_files_and_directory_trees() {
        let temp = TempDir::create();
        let file = StagingPath::sibling(&temp.path().join("file"), StagingKind::Upload);
        tokio::fs::write(file.path(), b"partial").await.unwrap();
        let directory =
            StagingPath::sibling(&temp.path().join("dir"), StagingKind::UploadDirectory);
        tokio::fs::create_dir_all(directory.path().join("nested"))
            .await
            .unwrap();
        let (file_path, directory_path) =
            (file.path().to_path_buf(), directory.path().to_path_buf());

        file.discard().await;
        directory.discard().await;

        // Awaited discard must finish before callers report a failure.
        assert!(!tokio::fs::try_exists(file_path).await.unwrap());
        // Directory staging is removed recursively through the guarded remover.
        assert!(!tokio::fs::try_exists(directory_path).await.unwrap());
    }

    /// Verifies kept paths are never removed.
    #[tokio::test]
    async fn keep_disarms_cleanup_after_publication() {
        let temp = TempDir::create();
        let staging = StagingPath::sibling(&temp.path().join("file"), StagingKind::LocalCopy);
        tokio::fs::write(staging.path(), b"published")
            .await
            .unwrap();

        let path = staging.keep();
        tokio::task::yield_now().await;

        // A kept path belongs to the caller again and must never be deleted behind its back.
        assert!(tokio::fs::try_exists(&path).await.unwrap());
    }

    /// Verifies the drop fallback removes staging of aborted owners.
    #[tokio::test]
    async fn dropping_an_armed_path_removes_it() {
        let temp = TempDir::create();
        let staging = StagingPath::sibling(&temp.path().join("file"), StagingKind::Upload);
        tokio::fs::write(staging.path(), b"aborted").await.unwrap();
        let path = staging.path().to_path_buf();

        drop(staging);

        // Aborted owners must not leak hidden partial output.
        wait_until_removed(&path).await;
    }
}
