//! Freedesktop.org trash for Linux and Android (Termux), which both expose `$HOME`.

use super::{TrashError, TrashProvider};
use crate::transfers::copy::CopyContext;
use futures_util::future::BoxFuture;
use jiff::{
    Zoned,
    civil::DateTime,
    tz::{AmbiguousOffset, TimeZone},
};
use redoor_core::{
    atomic_rename::{AtomicRenameOutcome, rename_without_replacement},
    log,
};
use redoor_protocol::{
    api::{
        transfers::CopyExistingMode,
        trash::{TrashItem, TrashListResponse, TrashLocation},
    },
    ids::UnixTimestampSeconds,
    wire::{
        command::{CommandErrorKind, MoveSourceIdentity},
        log::Level,
    },
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Component, Path, PathBuf},
};
use tokio::io::AsyncWriteExt;

/// Internal location data retains paths needed for secure metadata reconstruction and restore.
struct Location {
    id: String,
    root: PathBuf,
    mount_top: Option<PathBuf>,
    display_path: String,
}

/// Fresh inventory entries bind opaque API ids back to exact provider-owned paths.
struct InventoryItem {
    public: TrashItem,
    payload: PathBuf,
    info: PathBuf,
}

/// Creates a forced or private per-user root without accepting symlinked directories.
async fn prepare_private_root(root: &Path) -> Result<(), TrashError> {
    if !root.is_absolute() {
        return Err(TrashError::new(
            CommandErrorKind::InvalidInput,
            "Trash directory must be absolute",
        ));
    }
    create_private_directory_tree(root).await?;
    create_private_directory(&root.join("files")).await?;
    create_private_directory(&root.join("info")).await?;
    validate_private_root(root).await
}

/// Creates missing path components while refusing to traverse an existing symlink.
async fn create_private_directory_tree(path: &Path) -> Result<(), TrashError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if current == Path::new("/") {
            continue;
        }
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(metadata) => {
                let kind = if metadata.file_type().is_symlink() {
                    "a symlink"
                } else {
                    "not a directory"
                };
                return Err(TrashError::new(
                    CommandErrorKind::PermissionDenied,
                    format!(
                        "Trash directory path component {} is {kind}",
                        current.display()
                    ),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_private_directory(&current).await?;
            }
            Err(error) => {
                return Err(TrashError::io(
                    &format!(
                        "Failed to inspect trash directory path {}",
                        current.display()
                    ),
                    error,
                ));
            }
        }
    }
    Ok(())
}

/// Freedesktop provider, optionally pinned to one operator-selected root.
pub(super) struct FreedesktopTrash {
    /// Exact same-device root used instead of home and per-mount discovery.
    forced_root: Option<PathBuf>,
}

impl FreedesktopTrash {
    /// Validates an operator-selected root before the agent advertises trash support.
    pub(super) async fn initialize(forced_root: Option<PathBuf>) -> Result<Self, TrashError> {
        if let Some(root) = &forced_root {
            prepare_private_root(root).await?;
        }
        Ok(Self { forced_root })
    }
}

impl TrashProvider for FreedesktopTrash {
    /// Freedesktop trash supports moving entries.
    fn supports_move(&self) -> bool {
        true
    }

    /// Redoor can read and restore the freedesktop inventory format.
    fn supports_inventory(&self) -> bool {
        true
    }

    /// Moves one entry into the matching home or per-mount trash.
    fn trash(&self, path: PathBuf) -> BoxFuture<'_, Result<(), TrashError>> {
        Box::pin(self.trash_entry(path))
    }

    /// Lists every discoverable trash location.
    fn list(&self) -> BoxFuture<'_, Result<TrashListResponse, TrashError>> {
        Box::pin(self.list_locations())
    }

    /// Purges every discoverable trash location.
    fn empty(&self) -> BoxFuture<'_, Result<u64, TrashError>> {
        Box::pin(self.empty_locations())
    }

    /// Restores one item with smart-move semantics.
    fn restore<'a>(
        &'a self,
        location_id: &'a str,
        item_id: &'a str,
        destination: PathBuf,
        copy: CopyContext<'a>,
    ) -> BoxFuture<'a, Result<PathBuf, TrashError>> {
        Box::pin(self.restore_item(location_id, item_id, destination, copy))
    }
}

impl FreedesktopTrash {
    /// Moves a source only after the selected root and same-device relationship are revalidated.
    async fn trash_entry(&self, path: PathBuf) -> Result<(), TrashError> {
        let source = normalized_existing_entry(&path).await?;
        log!(
            Level::Debug,
            "Trash source normalized: requested_path={}, source={}",
            path.display(),
            source.display()
        );
        let source_metadata = tokio::fs::symlink_metadata(&source)
            .await
            .map_err(|error| TrashError::io("Failed to inspect trash source", error))?;
        log!(
            Level::Debug,
            "Trash source inspected: source={}, device={}, inode={}, directory={}, symlink={}, size={}",
            source.display(),
            source_metadata.dev(),
            source_metadata.ino(),
            source_metadata.is_dir(),
            source_metadata.file_type().is_symlink(),
            source_metadata.len()
        );
        let location = self.select_location(&source, &source_metadata).await?;
        log!(
            Level::Debug,
            "Trash location selected: source={}, location_id={}, root={}, mount_top={}",
            source.display(),
            location.id,
            location.root.display(),
            location
                .mount_top
                .as_deref()
                .map_or_else(|| "none".to_string(), |path| path.display().to_string())
        );
        validate_private_root(&location.root).await?;
        reject_trash_containment(&source, &location.root)?;
        let root_metadata = tokio::fs::metadata(&location.root)
            .await
            .map_err(|error| TrashError::io("Failed to inspect trash root", error))?;
        if source_metadata.dev() != root_metadata.dev() {
            return Err(TrashError::new(
                CommandErrorKind::InvalidInput,
                "Trash source and trash directory are on different devices",
            ));
        }

        let basename = source.file_name().ok_or_else(|| {
            TrashError::new(
                CommandErrorKind::InvalidInput,
                "Filesystem roots cannot be trashed",
            )
        })?;
        if basename.as_bytes().is_empty() || basename.as_bytes().contains(&b'/') {
            return Err(TrashError::new(
                CommandErrorKind::InvalidInput,
                "Trash source has an invalid filename",
            ));
        }
        let metadata_path = metadata_original_path(&source, location.mount_top.as_deref())?;
        log!(
            Level::Debug,
            "Trash source validated: source={}, metadata_path={}, trash_device={}",
            source.display(),
            metadata_path.display(),
            root_metadata.dev()
        );
        let deletion_date = Zoned::now().strftime("%Y-%m-%dT%H:%M:%S").to_string();
        for attempt in 0..1024_u32 {
            let name = collision_name(basename, attempt);
            let payload = location.root.join("files").join(&name);
            let info = trashinfo_path(&location.root, &name);
            let mut file = match tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&info)
                .await
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    log!(
                        Level::Debug,
                        "Trash name collision: source={}, candidate={}, attempt={}",
                        source.display(),
                        name.to_string_lossy(),
                        attempt
                    );
                    continue;
                }
                Err(error) => {
                    return Err(TrashError::io("Failed to reserve trash metadata", error));
                }
            };
            log!(
                Level::Debug,
                "Trash metadata reserved: source={}, candidate={}, info={}",
                source.display(),
                name.to_string_lossy(),
                info.display()
            );
            let contents = format!(
                "[Trash Info]
Path={}
DeletionDate={}
",
                percent_encode(metadata_path.as_os_str().as_bytes()),
                deletion_date
            );
            if let Err(error) = async {
                file.write_all(contents.as_bytes()).await?;
                file.sync_all().await
            }
            .await
            {
                let _ = tokio::fs::remove_file(&info).await;
                return Err(TrashError::io("Failed to write trash metadata", error));
            }
            drop(file);
            log!(
                Level::Debug,
                "Trash metadata published: source={}, info={}, payload={}",
                source.display(),
                info.display(),
                payload.display()
            );

            log!(
                Level::Debug,
                "Trash payload rename started: source={}, payload={}, source_device={}, trash_device={}, copy_fallback=false",
                source.display(),
                payload.display(),
                source_metadata.dev(),
                root_metadata.dev()
            );
            match rename_without_replacement(&source, &payload).await {
                Ok(AtomicRenameOutcome::Renamed) => {
                    log!(
                        Level::Debug,
                        "Trash payload moved: source={}, payload={}, info={}, method=filesystem_rename, copy_fallback=false",
                        source.display(),
                        payload.display(),
                        info.display()
                    );
                    return Ok(());
                }
                Ok(AtomicRenameOutcome::DestinationExists) => {
                    log!(
                        Level::Debug,
                        "Trash payload candidate already exists: source={}, payload={}, attempt={}",
                        source.display(),
                        payload.display(),
                        attempt
                    );
                    let _ = tokio::fs::remove_file(&info).await;
                    continue;
                }
                Ok(AtomicRenameOutcome::Unsupported) => {
                    let _ = tokio::fs::remove_file(&info).await;
                    return Err(TrashError::new(
                        CommandErrorKind::InvalidInput,
                        "Trash filesystem does not support no-replacement publication",
                    ));
                }
                Ok(AtomicRenameOutcome::Missing) => {
                    let _ = tokio::fs::remove_file(&info).await;
                    return Err(TrashError::new(
                        CommandErrorKind::NotFound,
                        format!(
                            "Trash source or selected trash directory no longer exists during rename: source={}, payload={}",
                            source.display(),
                            payload.display()
                        ),
                    ));
                }
                Ok(AtomicRenameOutcome::CrossDevice) => {
                    let _ = tokio::fs::remove_file(&info).await;
                    return Err(TrashError::new(
                        CommandErrorKind::InvalidInput,
                        format!(
                            "Trash rename crossed filesystem boundaries despite device validation: source={}, payload={}, source_device={}, trash_device={}",
                            source.display(),
                            payload.display(),
                            source_metadata.dev(),
                            root_metadata.dev()
                        ),
                    ));
                }
                Err(error) => {
                    let _ = tokio::fs::remove_file(&info).await;
                    return Err(TrashError::io(
                        &format!(
                            "Filesystem rename into trash failed (copy fallback is disabled): source={}, payload={}",
                            source.display(),
                            payload.display()
                        ),
                        error,
                    ));
                }
            }
        }
        Err(TrashError::new(
            CommandErrorKind::AlreadyExists,
            "Failed to reserve a unique trash item name",
        ))
    }

    /// Lists only direct payload entries with trustworthy deletion timestamps.
    async fn list_locations(&self) -> Result<TrashListResponse, TrashError> {
        let locations = self.discover_locations().await?;
        let mut public_locations = Vec::new();
        for location in locations {
            let mut items = inventory_for_location(&location).await?;
            items.sort_by(|left, right| {
                right
                    .public
                    .deleted_at
                    .cmp(&left.public.deleted_at)
                    .then_with(|| left.public.id.cmp(&right.public.id))
            });
            if !items.is_empty() {
                public_locations.push(TrashLocation {
                    id: location.id,
                    path: location.display_path,
                    items: items.into_iter().map(|item| item.public).collect(),
                });
            }
        }
        public_locations.sort_by(|left, right| {
            right.items[0]
                .deleted_at
                .cmp(&left.items[0].deleted_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(TrashListResponse {
            locations: public_locations,
        })
    }

    /// Removes every direct payload and metadata entry without deleting provider-owned directories.
    async fn empty_locations(&self) -> Result<u64, TrashError> {
        let locations = self.discover_locations().await?;
        let mut deleted_items = 0_u64;
        for location in locations {
            validate_private_root(&location.root).await?;
            deleted_items += remove_directory_contents(&location.root.join("files"), true).await?;
            remove_directory_contents(&location.root.join("info"), false).await?;
        }
        Ok(deleted_items)
    }

    /// Restores a freshly resolved item without replacing an occupied selected destination.
    async fn restore_item(
        &self,
        location_id: &str,
        item_id: &str,
        destination: PathBuf,
        copy: CopyContext<'_>,
    ) -> Result<PathBuf, TrashError> {
        validate_identifier(location_id)?;
        validate_identifier(item_id)?;
        if !destination.is_absolute() || destination == Path::new("/") {
            return Err(TrashError::new(
                CommandErrorKind::InvalidInput,
                "Trash restore destination must be an absolute non-root path",
            ));
        }
        let locations = self.discover_locations().await?;
        let location = locations
            .into_iter()
            .find(|location| location.id == location_id)
            .ok_or_else(|| {
                TrashError::new(CommandErrorKind::NotFound, "Trash location not found")
            })?;
        validate_private_root(&location.root).await?;
        let item = inventory_for_location(&location)
            .await?
            .into_iter()
            .find(|item| item.public.id == item_id)
            .ok_or_else(|| TrashError::new(CommandErrorKind::NotFound, "Trash item not found"))?;
        let parent = destination.parent().ok_or_else(|| {
            TrashError::new(
                CommandErrorKind::InvalidInput,
                "Trash restore destination has no parent",
            )
        })?;
        let parent_directory = tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent)
            .await
            .map_err(|error| {
                if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
                    TrashError::new(
                        CommandErrorKind::NotADirectory,
                        "Trash restore parent must be an existing real directory",
                    )
                } else {
                    TrashError::io("Failed to open restore parent", error)
                }
            })?;
        let payload_metadata = tokio::fs::symlink_metadata(&item.payload)
            .await
            .map_err(|error| TrashError::io("Failed to inspect trash payload", error))?;
        let atomic = copy
            .move_entry(
                item.payload.clone(),
                destination.clone(),
                payload_metadata.is_dir(),
                MoveSourceIdentity::from_metadata(&payload_metadata),
                CopyExistingMode::Error,
            )
            .await
            .map_err(|error| {
                TrashError::new(
                    error.kind(),
                    format!("Failed to restore trash payload with smart move: {error}"),
                )
            })?;
        drop(parent_directory);
        log!(
            Level::Debug,
            "Trash payload restored: payload={}, destination={}, method={}, copy_fallback={}",
            item.payload.display(),
            destination.display(),
            if atomic {
                "filesystem_rename"
            } else {
                "transfer_copy_delete"
            },
            !atomic
        );
        if let Err(error) = tokio::fs::remove_file(&item.info).await {
            redoor_core::log_failure!(
                Level::Error,
                "Failed to remove trash metadata after restore: path={}, error={}",
                item.info.display(),
                error
            );
            return Err(TrashError::io(
                "Trash item restored but metadata cleanup failed",
                error,
            ));
        }
        Ok(destination)
    }

    /// Selects the forced root, home trash, or a secure per-mount user trash.
    async fn select_location(
        &self,
        source: &Path,
        source_metadata: &std::fs::Metadata,
    ) -> Result<Location, TrashError> {
        if let Some(root) = self.forced_root.as_deref() {
            return location(root.to_path_buf(), None);
        }
        let home_root = home_trash_root(&home_directory()?);
        let home_trash_device = nearest_existing_ancestor_device(&home_root).await?;
        if source_metadata.dev() == home_trash_device {
            let root = home_root;
            prepare_private_root(&root).await?;
            return location(root, None);
        }
        let mount_top = mount_top_for(source, source_metadata.dev()).await?;
        let root = mount_trash_root(&mount_top).await?;
        prepare_private_root(&root).await?;
        location(root, Some(mount_top))
    }

    /// Discovers only roots that can be safely validated at listing time.
    async fn discover_locations(&self) -> Result<Vec<Location>, TrashError> {
        if let Some(root) = self.forced_root.as_deref() {
            validate_private_root(root).await?;
            return Ok(vec![location(root.to_path_buf(), None)?]);
        }
        let home = home_directory()?;
        let home_root = home_trash_root(&home);
        let mut locations = Vec::new();
        let mut seen = HashSet::new();
        if validate_private_root(&home_root).await.is_ok() {
            seen.insert(home_root.clone());
            locations.push(location(home_root, None)?);
        }
        for mount_top in mount_points().await? {
            let uid = nix::unistd::Uid::effective().as_raw();
            let shared = mount_top.join(".Trash");
            let shared_user = shared.join(uid.to_string());
            if validate_shared_trash(&shared).await
                && seen.insert(shared_user.clone())
                && validate_private_root(&shared_user).await.is_ok()
            {
                locations.push(location(shared_user, Some(mount_top.clone()))?);
            }
            let private = mount_top.join(format!(".Trash-{uid}"));
            if seen.insert(private.clone()) && validate_private_root(&private).await.is_ok() {
                locations.push(location(private, Some(mount_top.clone()))?);
            }
        }
        Ok(locations)
    }
}

/// Deletes one directory's direct entries, using guarded recursion only for real directories.
async fn remove_directory_contents(path: &Path, count_entries: bool) -> Result<u64, TrashError> {
    let mut entries = tokio::fs::read_dir(path)
        .await
        .map_err(|error| TrashError::io("Failed to open trash directory", error))?;
    let mut removed = 0_u64;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| TrashError::io("Failed to read trash directory", error))?
    {
        let file_type = entry
            .file_type()
            .await
            .map_err(|error| TrashError::io("Failed to inspect trash entry", error))?;
        if file_type.is_dir() && !file_type.is_symlink() {
            redoor_core::safe_fs::safe_rm_all(entry.path())
                .await
                .map_err(|error| TrashError::io("Failed to remove trash directory", error))?;
        } else {
            tokio::fs::remove_file(entry.path())
                .await
                .map_err(|error| TrashError::io("Failed to remove trash entry", error))?;
        }
        if count_entries {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Reads paired metadata without following or recursively examining payload entries.
async fn inventory_for_location(location: &Location) -> Result<Vec<InventoryItem>, TrashError> {
    let mut directory = tokio::fs::read_dir(location.root.join("files"))
        .await
        .map_err(|error| TrashError::io("Failed to read trash payload directory", error))?;
    let mut items = Vec::new();
    while let Some(entry) = directory
        .next_entry()
        .await
        .map_err(|error| TrashError::io("Failed to read trash payload entry", error))?
    {
        let name_os = entry.file_name();
        let Some(name) = name_os.to_str().map(str::to_owned) else {
            continue;
        };
        let info = trashinfo_path(&location.root, &name_os);
        let contents = match tokio::fs::read_to_string(&info).await {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                log!(
                    Level::Warning,
                    "Ignoring unreadable trash metadata: path={}, error={}",
                    info.display(),
                    error
                );
                continue;
            }
        };
        let Some((deleted_at, original)) =
            parse_trashinfo(&contents, location.mount_top.as_deref())
        else {
            continue;
        };
        let original_path = original
            .as_ref()
            .and_then(|path| path.to_str().map(str::to_owned));
        items.push(InventoryItem {
            public: TrashItem {
                id: opaque_id(&[location.root.as_os_str().as_bytes(), name.as_bytes()]),
                name,
                original_path,
                deleted_at: UnixTimestampSeconds::new(deleted_at),
            },
            payload: entry.path(),
            info,
        });
    }
    Ok(items)
}

/// Parses only the freedesktop fields needed for ordering and a safe suggested path.
fn parse_trashinfo(contents: &str, mount_top: Option<&Path>) -> Option<(i64, Option<PathBuf>)> {
    let mut path = None;
    let mut deletion_date = None;
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("Path=") {
            path = strict_percent_decode(value.as_bytes());
        } else if let Some(value) = line.strip_prefix("DeletionDate=") {
            deletion_date = DateTime::strptime("%Y-%m-%dT%H:%M:%S", value)
                .ok()
                .and_then(|date| {
                    let ambiguous = TimeZone::system().to_ambiguous_timestamp(date);
                    // A skipped local time is invalid metadata; repeated times use the first instant.
                    if matches!(ambiguous.offset(), AmbiguousOffset::Gap { .. }) {
                        return None;
                    }
                    ambiguous.earlier().ok()
                })
                .map(|date| date.as_second());
        }
    }
    let deleted_at = deletion_date?;
    let original = path.and_then(|bytes| safe_original_path(bytes, mount_top));
    Some((deleted_at, original))
}

/// Prevents mount-relative metadata from escaping the mount or absolute metadata from being relative.
fn safe_original_path(bytes: Vec<u8>, mount_top: Option<&Path>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    match mount_top {
        Some(mount) if is_safe_relative(&path) => Some(mount.join(path)),
        None if path.is_absolute() => Some(path),
        _ => None,
    }
}

/// Allows only normal relative components in mount-specific metadata.
fn is_safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Converts a source to the absolute metadata form required for its trash location.
fn metadata_original_path(source: &Path, mount_top: Option<&Path>) -> Result<PathBuf, TrashError> {
    match mount_top {
        Some(mount) => source
            .strip_prefix(mount)
            .map(Path::to_path_buf)
            .map_err(|_| {
                TrashError::new(
                    CommandErrorKind::InvalidInput,
                    "Trash source is outside its selected mount",
                )
            }),
        None => Ok(source.to_path_buf()),
    }
}

/// Canonicalizes only the parent so a symlink source is moved rather than followed.
async fn normalized_existing_entry(path: &Path) -> Result<PathBuf, TrashError> {
    if !path.is_absolute() || path == Path::new("/") {
        return Err(TrashError::new(
            CommandErrorKind::InvalidInput,
            "Trash source must be an absolute non-root path",
        ));
    }
    let name = path.file_name().ok_or_else(|| {
        TrashError::new(
            CommandErrorKind::InvalidInput,
            "Trash source has no filename",
        )
    })?;
    let parent = path.parent().ok_or_else(|| {
        TrashError::new(CommandErrorKind::InvalidInput, "Trash source has no parent")
    })?;
    let parent = tokio::fs::canonicalize(parent)
        .await
        .map_err(|error| TrashError::io("Failed to resolve trash source parent", error))?;
    Ok(parent.join(name))
}

/// Prevents moving a trash root or any payload already stored below it.
fn reject_trash_containment(source: &Path, root: &Path) -> Result<(), TrashError> {
    if source == root || source.starts_with(root) {
        return Err(TrashError::new(
            CommandErrorKind::InvalidInput,
            "Trash directories and their contents cannot be trashed",
        ));
    }
    Ok(())
}

/// Builds a collision candidate without requiring UTF-8 source filenames.
fn collision_name(name: &std::ffi::OsStr, attempt: u32) -> std::ffi::OsString {
    if attempt == 0 {
        return name.to_os_string();
    }
    let mut bytes = name.as_bytes().to_vec();
    bytes.extend_from_slice(format!(".{attempt}").as_bytes());
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(bytes)
}

/// Appends the metadata suffix as raw bytes so non-UTF-8 payload names remain paired safely.
fn trashinfo_path(root: &Path, name: &std::ffi::OsStr) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let mut bytes = name.as_bytes().to_vec();
    bytes.extend_from_slice(b".trashinfo");
    root.join("info").join(std::ffi::OsString::from_vec(bytes))
}

/// Percent-encodes path bytes according to the trash specification's URL path representation.
fn percent_encode(bytes: &[u8]) -> String {
    let mut encoded = String::new();
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Rejects malformed escapes instead of partially decoding attacker-controlled metadata.
fn strict_percent_decode(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push(hex_value(high)? * 16 + hex_value(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Some(decoded)
}

/// Decodes one ASCII hexadecimal digit for strict percent parsing.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Produces deterministic opaque ids without exposing provider paths or filenames.
fn opaque_id(parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part);
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Rejects malformed identifiers before filesystem discovery.
fn validate_identifier(identifier: &str) -> Result<(), TrashError> {
    if identifier.len() == 64 && identifier.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(TrashError::new(
            CommandErrorKind::InvalidInput,
            "Trash identifiers must be opaque values returned by listing",
        ))
    }
}

/// Creates one API/internal location pair from a validated provider root.
fn location(root: PathBuf, mount_top: Option<PathBuf>) -> Result<Location, TrashError> {
    let display_path = root.to_str().ok_or_else(|| {
        TrashError::new(
            CommandErrorKind::InvalidInput,
            "Trash location is not valid UTF-8",
        )
    })?;
    Ok(Location {
        id: opaque_id(&[root.as_os_str().as_bytes()]),
        display_path: display_path.to_string(),
        root,
        mount_top,
    })
}

/// Finds the user's home without guessing a root-owned fallback.
fn home_directory() -> Result<PathBuf, TrashError> {
    redoor_core::paths::home()
        .map_err(|error| TrashError::new(CommandErrorKind::InvalidInput, error.to_string()))
}

/// Applies XDG_DATA_HOME only when it is an absolute path as required by XDG.
fn home_trash_root(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"))
        .join("Trash")
}

/// Finds the device that will contain a potentially not-yet-created trash root.
async fn nearest_existing_ancestor_device(path: &Path) -> Result<u64, TrashError> {
    let mut candidate = path;
    loop {
        match tokio::fs::metadata(candidate).await {
            Ok(metadata) => return Ok(metadata.dev()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                candidate = candidate.parent().ok_or_else(|| {
                    TrashError::new(
                        CommandErrorKind::InvalidInput,
                        "Trash directory has no existing filesystem ancestor",
                    )
                })?;
            }
            Err(error) => {
                return Err(TrashError::io(
                    "Failed to inspect trash directory ancestor",
                    error,
                ));
            }
        }
    }
}

/// Selects the shared sticky trash only when secure, otherwise the private fallback.
async fn mount_trash_root(mount_top: &Path) -> Result<PathBuf, TrashError> {
    let uid = nix::unistd::Uid::effective().as_raw();
    let shared = mount_top.join(".Trash");
    if validate_shared_trash(&shared).await {
        Ok(shared.join(uid.to_string()))
    } else {
        Ok(mount_top.join(format!(".Trash-{uid}")))
    }
}

/// Accepts only a real sticky directory for the shared mount trash convention.
async fn validate_shared_trash(path: &Path) -> bool {
    tokio::fs::symlink_metadata(path)
        .await
        .is_ok_and(|metadata| {
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.mode() & libc::S_ISVTX != 0
        })
}

/// Creates one directory with private permissions, tolerating an existing validated directory.
async fn create_private_directory(path: &Path) -> Result<(), TrashError> {
    match tokio::fs::DirBuilder::new().mode(0o700).create(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(TrashError::io(
            &format!("Failed to create trash directory {}", path.display()),
            error,
        )),
    }
}

/// Revalidates ownership, type, permissions, and child device identity before destructive rename.
async fn validate_private_root(root: &Path) -> Result<(), TrashError> {
    let uid = nix::unistd::Uid::effective().as_raw();
    let root_metadata = tokio::fs::symlink_metadata(root).await.map_err(|error| {
        TrashError::io(
            &format!("Failed to inspect trash directory {}", root.display()),
            error,
        )
    })?;
    if let Some(problem) = private_directory_problem(&root_metadata, uid, None) {
        return Err(TrashError::new(
            CommandErrorKind::PermissionDenied,
            format!(
                "Trash directory {} must be a private real directory owned by the agent user: {problem}",
                root.display()
            ),
        ));
    }
    for child in [root.join("files"), root.join("info")] {
        let metadata = tokio::fs::symlink_metadata(&child).await.map_err(|error| {
            TrashError::io(
                &format!(
                    "Failed to inspect trash child directory {}",
                    child.display()
                ),
                error,
            )
        })?;
        if let Some(problem) = private_directory_problem(&metadata, uid, Some(root_metadata.dev()))
        {
            return Err(TrashError::new(
                CommandErrorKind::PermissionDenied,
                format!(
                    "Trash child directory {} must be private and on the trash device: {problem}",
                    child.display()
                ),
            ));
        }
    }
    Ok(())
}

/// Names the first failed private-directory rule so operators know whether to fix the
/// owner, the mode, or the path itself instead of guessing from a generic refusal.
fn private_directory_problem(
    metadata: &std::fs::Metadata,
    uid: u32,
    expected_device: Option<u64>,
) -> Option<String> {
    let mode = metadata.mode() & 0o7777;
    if metadata.file_type().is_symlink() {
        Some("it is a symlink".to_string())
    } else if !metadata.is_dir() {
        Some("it is not a directory".to_string())
    } else if metadata.uid() != uid {
        Some(format!(
            "it is owned by uid {} but the agent runs as uid {uid}",
            metadata.uid()
        ))
    } else if mode & 0o077 != 0 {
        Some(format!(
            "its mode is {mode:04o} but group and others must have no access (expected 0700)"
        ))
    } else if let Some(device) = expected_device
        && metadata.dev() != device
    {
        Some(format!(
            "it is on device {} but the trash directory is on device {device}",
            metadata.dev()
        ))
    } else {
        None
    }
}

/// Chooses the longest containing mount point whose device id matches the source.
async fn mount_top_for(source: &Path, device: u64) -> Result<PathBuf, TrashError> {
    let mut candidates = mount_points().await?;
    candidates.sort_by_key(|path| std::cmp::Reverse(path.as_os_str().as_bytes().len()));
    for mount in candidates {
        if source.starts_with(&mount)
            && tokio::fs::metadata(&mount)
                .await
                .is_ok_and(|metadata| metadata.dev() == device)
        {
            return Ok(mount);
        }
    }
    Err(TrashError::new(
        CommandErrorKind::InvalidInput,
        "Failed to discover the source filesystem mount point",
    ))
}

/// Uses platform mount discovery without filtering valid writable filesystems.
async fn mount_points() -> Result<Vec<PathBuf>, TrashError> {
    crate::platform::mounts::paths()
        .await
        .map_err(|message| TrashError::new(CommandErrorKind::Internal, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use redoor_core::test_support::TempDir;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn percent_encoding_round_trips_arbitrary_path_bytes() {
        let bytes = b"/tmp/a b/%/\xFF";
        let encoded = percent_encode(bytes);
        assert_eq!(
            strict_percent_decode(encoded.as_bytes()),
            Some(bytes.to_vec())
        );
    }

    #[test]
    fn malformed_percent_escapes_are_rejected() {
        assert_eq!(strict_percent_decode(b"/tmp/%GG"), None);
        assert_eq!(strict_percent_decode(b"/tmp/%1"), None);
    }

    #[test]
    fn mount_relative_paths_cannot_escape_the_mount() {
        assert!(safe_original_path(b"dir/file".to_vec(), Some(Path::new("/mnt"))).is_some());
        assert!(safe_original_path(b"../file".to_vec(), Some(Path::new("/mnt"))).is_none());
        assert!(safe_original_path(b"/file".to_vec(), Some(Path::new("/mnt"))).is_none());
    }

    #[test]
    fn identifiers_have_a_fixed_opaque_shape() {
        let identifier = opaque_id(&[b"location", b"item"]);
        assert!(validate_identifier(&identifier).is_ok());
        assert!(validate_identifier("../files/item").is_err());
    }

    #[test]
    fn deletion_dates_are_interpreted_in_local_time() {
        let naive = DateTime::strptime("%Y-%m-%dT%H:%M:%S", "2020-01-01T00:00:00").unwrap();
        let expected = TimeZone::system()
            .to_ambiguous_timestamp(naive)
            .earlier()
            .unwrap();

        let parsed = parse_trashinfo(
            "[Trash Info]
Path=/tmp/file
DeletionDate=2020-01-01T00:00:00
",
            None,
        )
        .unwrap();

        // Freedesktop metadata omits an offset, so parsing must use the host's local timezone.
        assert_eq!(parsed.0, expected.as_second());
    }

    #[tokio::test]
    async fn private_root_creation_refuses_symlinked_components() {
        let temp_dir = TempDir::create();
        let real = temp_dir.path().join("real");
        let link = temp_dir.path().join("link");
        tokio::fs::create_dir(&real).await.unwrap();
        tokio::fs::symlink(&real, &link).await.unwrap();

        let result = prepare_private_root(&link.join("Trash")).await;

        assert!(
            result.is_err(),
            "trash root creation must never traverse a symlinked path component"
        );
        assert!(
            !tokio::fs::try_exists(real.join("Trash")).await.unwrap(),
            "refusing the symlink must leave its target untouched"
        );
    }

    #[tokio::test]
    async fn private_root_validation_reports_the_path_and_failed_rule() {
        let temp_dir = TempDir::create();
        let root = temp_dir.path().join("Trash");
        prepare_private_root(&root).await.unwrap();
        tokio::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();

        let error = validate_private_root(&root).await.unwrap_err();

        // Operators need the exact path to know which trash directory to fix.
        assert!(error.message.contains(&root.display().to_string()));
        // Naming the offending mode tells operators a chmod fixes it, not a chown.
        assert!(error.message.contains("its mode is 0755"));
    }

    #[tokio::test]
    async fn home_trash_device_uses_the_nearest_existing_ancestor() {
        let temp_dir = TempDir::create();
        let missing_root = temp_dir.path().join("data/missing/Trash");
        let expected_device = tokio::fs::metadata(temp_dir.path()).await.unwrap().dev();

        let device = nearest_existing_ancestor_device(&missing_root)
            .await
            .unwrap();

        assert_eq!(
            device, expected_device,
            "a missing XDG trash root must inherit the filesystem of its existing ancestor"
        );
    }
}
