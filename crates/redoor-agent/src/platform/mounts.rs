//! Mounted filesystem discovery.
//!
//! The `mountpoints` crate has no Android backend, so Termux agents report no mount capacity
//! and discover no per-mount trash directories; home trash still works there.

use redoor_protocol::api::agents::MountPoint;

/// Lists visible mounts with their capacity, sorted by path, for the agent details view.
///
/// Mount discovery uses blocking platform APIs, so it runs off the async worker threads.
#[cfg(not(target_os = "android"))]
pub(crate) async fn capacities() -> Result<Vec<MountPoint>, String> {
    let mut mounts = match tokio::task::spawn_blocking(mountpoints::mountinfos).await {
        Ok(Ok(mount_infos)) => mount_infos
            .into_iter()
            .map(|mount_info| MountPoint {
                path: mount_info.path.to_string_lossy().into_owned(),
                available_bytes: mount_info.avail,
                total_bytes: mount_info.size,
                mount_type: mount_info.format,
            })
            .filter(MountPoint::is_visible)
            .collect::<Vec<_>>(),
        Ok(Err(error)) => return Err(format!("Failed to list mount points: {error:?}")),
        Err(error) => return Err(format!("Mount point task failed: {error}")),
    };
    mounts.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(mounts)
}

/// Capacity is unavailable on Android because Bionic exposes no supported enumeration API.
#[cfg(target_os = "android")]
pub(crate) async fn capacities() -> Result<Vec<MountPoint>, String> {
    Ok(Vec::new())
}

/// Lists every mount root (including hidden ones), because any writable mount may hold a
/// per-mount trash directory.
#[cfg(target_os = "linux")]
pub(crate) async fn paths() -> Result<Vec<std::path::PathBuf>, String> {
    tokio::task::spawn_blocking(mountpoints::mountpaths)
        .await
        .map_err(|error| format!("Mount point task failed: {error}"))?
        .map_err(|error| format!("Failed to list mount points: {error}"))
}

/// Only the home trash is discoverable on Android.
#[cfg(target_os = "android")]
pub(crate) async fn paths() -> Result<Vec<std::path::PathBuf>, String> {
    Ok(Vec::new())
}
