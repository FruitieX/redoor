//! Path of the redoor binary that stays usable after `redoor upgrade` replaced it.
//!
//! An upgrade renames a new binary over the old path while servers and agents keep running.
//! Linux then reports the running image as `<path> (deleted)` through `/proc/self/exe`, so a
//! plain `current_exe()` names a file that does not exist. Executing the path itself instead
//! keeps re-exec, managed agent launches and SSH askpass working, and always runs the newest
//! binary installed there.

use std::io;
use std::path::PathBuf;

/// Suffix Linux appends to `/proc/<pid>/exe` once the file behind it was unlinked.
pub const REPLACED_SUFFIX: &str = " (deleted)";

/// The path this process was executed from, which holds the upgraded binary after an upgrade.
pub fn current() -> io::Result<PathBuf> {
    Ok(without_replaced_suffix(std::env::current_exe()?))
}

/// Strips the marker Linux adds once a running binary was replaced on disk.
pub fn without_replaced_suffix(path: PathBuf) -> PathBuf {
    if !cfg!(any(target_os = "linux", target_os = "android")) {
        return path;
    }
    match path
        .to_str()
        .and_then(|text| text.strip_suffix(REPLACED_SUFFIX))
    {
        Some(stripped) => PathBuf::from(stripped),
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-exec after an upgrade must target the real path, not Linux's replaced marker.
    #[test]
    fn strips_the_replaced_marker_on_linux() {
        let stripped = without_replaced_suffix(PathBuf::from("/usr/bin/redoor (deleted)"));
        if cfg!(any(target_os = "linux", target_os = "android")) {
            // Linux appends the marker, so it must be removed to name the upgraded file.
            assert_eq!(stripped, PathBuf::from("/usr/bin/redoor"));
        } else {
            // Other platforms never append it, so the name is left alone.
            assert_eq!(stripped, PathBuf::from("/usr/bin/redoor (deleted)"));
        }
        // Paths without the marker pass through untouched.
        assert_eq!(
            without_replaced_suffix(PathBuf::from("/usr/bin/redoor")),
            PathBuf::from("/usr/bin/redoor")
        );
    }
}
