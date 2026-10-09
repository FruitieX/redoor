//! Agent-side implementations of the filesystem, search, git, and host commands.

pub(crate) mod content_grep;
pub(crate) mod file_search;
mod git;
pub mod handler;
pub mod identity;
mod metadata;

#[cfg(test)]
mod tests {
    use super::handler::CommandHandler;
    use redoor_core::identity::current_binary_identity;
    use redoor_core::test_support::TempDir;
    use redoor_protocol::{
        api::{
            agents::{EchoRequest, MountPoint},
            files::CreationOwnershipOptions,
            transfers::CopyExistingMode,
        },
        wire::command::{Command, CommandErrorKind, CommandResult},
    };

    #[tokio::test]
    async fn test_ls_command() {
        let handler = CommandHandler::new();
        let result = handler.execute(Command::Ls { path: None }).await;

        match result {
            CommandResult::LsDirectory(ls_result) => {
                assert!(!ls_result.files.is_empty(), "ls should return files");
                assert!(
                    !ls_result.path.is_empty(),
                    "directory path should be populated"
                );
                assert!(
                    ls_result.permissions > 0,
                    "directory permissions should be populated"
                );
                let first_file = &ls_result.files[0];
                assert!(
                    first_file.file_type == "file" || first_file.file_type == "directory",
                    "file_type should be 'file' or 'directory'"
                );
                assert!(first_file.uid > 0, "uid should be populated");
                assert!(first_file.gid > 0, "gid should be populated");
                assert!(!first_file.name.is_empty(), "name should not be empty");
            }
            _ => panic!("Expected LsDirectoryResult"),
        }
    }

    #[tokio::test]
    async fn test_ls_error_includes_requested_path() {
        let handler = CommandHandler::new();
        let path = "nonexistent-directory-for-ls-context";
        let result = handler
            .execute(Command::Ls {
                path: Some(path.to_string()),
            })
            .await;

        match result {
            CommandResult::Error { kind, message } => {
                // The stable kind lets the REST layer return a useful status without parsing text.
                assert_eq!(kind, CommandErrorKind::NotFound);
                // Naming the attempted path makes remote filesystem failures actionable.
                assert!(message.contains(path), "ls error should include its path");
            }
            _ => panic!("Expected Error"),
        }
    }

    #[tokio::test]
    async fn test_raw_download_command() {
        let handler = CommandHandler::new();
        let result = handler
            .execute(Command::RawDownload {
                path: "test.txt".to_string(),
                range_start: None,
                range_end: None,
            })
            .await;

        match result {
            CommandResult::RawDownload { path } => {
                assert_eq!(path, "test.txt");
            }
            _ => panic!("Expected RawDownload"),
        }
    }

    #[tokio::test]
    async fn test_tar_download_command() {
        let handler = CommandHandler::new();
        let result = handler
            .execute(Command::TarDownload {
                path: "test-dir".to_string(),
                include_root: false,
            })
            .await;

        match result {
            CommandResult::TarDownload { path } => {
                assert_eq!(path, "test-dir");
            }
            _ => panic!("Expected TarDownload"),
        }
    }

    #[tokio::test]
    async fn test_tar_upload_command() {
        let handler = CommandHandler::new();
        let result = handler
            .execute(Command::TarUpload {
                path: "test-dir".to_string(),
                on_existing: CopyExistingMode::Error,
            })
            .await;

        match result {
            CommandResult::TarUpload => {}
            _ => panic!("Expected TarUpload"),
        }
    }

    #[tokio::test]
    async fn test_raw_upload_command() {
        let handler = CommandHandler::new();
        let result = handler
            .execute(Command::RawUpload {
                path: "upload.txt".to_string(),
                on_existing: CopyExistingMode::Override,
                ownership: CreationOwnershipOptions::default(),
            })
            .await;

        match result {
            CommandResult::RawUpload => {}
            _ => panic!("Expected RawUpload"),
        }
    }

    #[tokio::test]
    async fn test_raw_delete_command() {
        let handler = CommandHandler::new();
        let temp = TempDir::create();
        let temp_path = temp.path().join("delete-test.txt");

        tokio::fs::write(&temp_path, "delete me")
            .await
            .expect("temporary file should be created");

        let result = handler
            .execute(Command::RawDelete {
                path: temp_path.to_string_lossy().to_string(),
            })
            .await;

        match result {
            CommandResult::RawDelete => {
                assert!(
                    !tokio::fs::try_exists(&temp_path)
                        .await
                        .expect("file existence should be queryable"),
                    "file should be removed"
                );
            }
            _ => panic!("Expected RawDelete"),
        }
    }

    #[tokio::test]
    async fn test_raw_delete_command_removes_directory_recursively() {
        let handler = CommandHandler::new();
        let temp = TempDir::create();
        let temp_dir = temp.path().join("delete-dir-test");
        let nested_dir = temp_dir.join("nested");
        let nested_file = nested_dir.join("file.txt");

        tokio::fs::create_dir_all(&nested_dir)
            .await
            .expect("temporary directory should be created");
        tokio::fs::write(&nested_file, "delete me")
            .await
            .expect("temporary nested file should be created");

        let result = handler
            .execute(Command::RawDelete {
                path: temp_dir.to_string_lossy().to_string(),
            })
            .await;

        match result {
            CommandResult::RawDelete => {
                assert!(
                    !tokio::fs::try_exists(&temp_dir)
                        .await
                        .expect("directory existence should be queryable"),
                    "directory should be removed recursively"
                );
            }
            _ => panic!("Expected RawDelete"),
        }
    }

    #[tokio::test]
    async fn test_echo_command() {
        let handler = CommandHandler::new();
        let result = handler
            .execute(Command::Echo {
                request: EchoRequest {
                    message: "hello world".to_string(),
                    random_sleep: false,
                },
            })
            .await;

        match result {
            CommandResult::Echo(echo_result) => {
                assert_eq!(echo_result.message, "hello world");
            }
            _ => panic!("Expected EchoResult"),
        }
    }

    #[tokio::test]
    async fn test_create_directory_command() {
        let handler = CommandHandler::new();
        let temp = TempDir::create();
        let nested_dir = temp.path().join("nested").join("child");

        let result = handler
            .execute(Command::CreateDirectory {
                path: nested_dir.to_string_lossy().to_string(),
                ownership: CreationOwnershipOptions::default(),
            })
            .await;

        match result {
            CommandResult::CreateDirectory => {
                assert!(
                    tokio::fs::try_exists(&nested_dir)
                        .await
                        .expect("directory existence should be queryable"),
                    "directory should be created recursively"
                );
            }
            _ => panic!("Expected CreateDirectory"),
        }
    }

    #[tokio::test]
    async fn test_rename_path_command() {
        let handler = CommandHandler::new();
        let temp = TempDir::create();
        let source_path = temp.path().join("before.txt");
        let dest_path = temp.path().join("after.txt");
        tokio::fs::write(&source_path, "rename me")
            .await
            .expect("source file should be created");

        let result = handler
            .execute(Command::RenamePath {
                dir: temp.path().to_string_lossy().to_string(),
                old: "before.txt".to_string(),
                new: "after.txt".to_string(),
            })
            .await;

        match result {
            CommandResult::RenamePath => {
                assert!(
                    !tokio::fs::try_exists(&source_path)
                        .await
                        .expect("source existence should be queryable"),
                    "the source path should disappear after rename"
                );
                assert_eq!(
                    tokio::fs::read_to_string(&dest_path)
                        .await
                        .expect("renamed file should remain readable"),
                    "rename me",
                    "rename should preserve file contents"
                );
            }
            _ => panic!("Expected RenamePath"),
        }
    }

    #[tokio::test]
    async fn test_chmod_path_preserves_special_bits() {
        let handler = CommandHandler::new();
        let temp = TempDir::create();
        let path = temp.path().join("mode.txt");
        tokio::fs::write(&path, "mode")
            .await
            .expect("chmod fixture should be created");
        let mut permissions = tokio::fs::metadata(&path)
            .await
            .expect("chmod fixture metadata should be readable")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o4644);
        tokio::fs::set_permissions(&path, permissions)
            .await
            .expect("setuid fixture mode should apply");

        let result = handler
            .execute(Command::ChmodPath {
                path: path.to_string_lossy().to_string(),
                permissions: 0o600,
            })
            .await;

        match result {
            CommandResult::ChmodPath(response) => {
                assert_eq!(
                    response.permissions, 0o600,
                    "the response must echo the ordinary bits the grid requested"
                );
                let mode = tokio::fs::metadata(&path)
                    .await
                    .expect("chmod result metadata should be readable")
                    .permissions();
                let mode = std::os::unix::fs::PermissionsExt::mode(&mode);
                assert_eq!(
                    mode & 0o777,
                    0o600,
                    "disk rwx bits must match the requested ordinary mode"
                );
                assert_eq!(
                    mode & 0o7000,
                    0o4000,
                    "setuid must survive an ordinary-mode chmod"
                );
            }
            other => panic!("Expected ChmodPath, got {}", other.summary()),
        }
    }

    #[tokio::test]
    async fn test_chown_path_rejects_non_root() {
        if nix::unistd::Uid::effective().is_root() {
            return;
        }
        let handler = CommandHandler::new();
        let temp = TempDir::create();
        let path = temp.path().join("owned.txt");
        tokio::fs::write(&path, "owned")
            .await
            .expect("chown fixture should be created");

        let result = handler
            .execute(Command::ChownPath {
                path: path.to_string_lossy().to_string(),
                owner: Some(String::from("0")),
                group: None,
            })
            .await;

        match result {
            CommandResult::Error {
                kind: CommandErrorKind::PermissionDenied,
                ..
            } => {}
            other => panic!(
                "non-root chown must be PermissionDenied, got {}",
                other.summary()
            ),
        }
    }

    #[tokio::test]
    async fn test_get_agent_details_command() {
        let handler = CommandHandler::new();
        let result = handler.execute(Command::GetAgentDetails).await;

        match result {
            CommandResult::GetAgentDetails(details) => {
                assert!(details.pid > 0, "PID should be positive");
                assert!(!details.cwd.is_empty(), "CWD should not be empty");
                // Unit tests do not call set_agent_loaded_config_path, so path stays empty.
                assert!(
                    details.config_path.is_empty(),
                    "config_path should be empty without agent startup registration"
                );
                // Absolute binary path lets operators confirm which file is running.
                assert!(
                    details.exe_path.starts_with('/'),
                    "exe_path should be absolute, got {}",
                    details.exe_path
                );
                assert!(!details.os.is_empty(), "OS should not be empty");
                assert!(!details.arch.is_empty(), "ARCH should not be empty");
                assert!(!details.hostname.is_empty(), "Hostname should not be empty");
                assert!(!details.username.is_empty(), "Username should not be empty");
                assert_eq!(
                    details.uid,
                    nix::unistd::Uid::effective().as_raw(),
                    "details uid must come from Uid::effective rather than $USER"
                );
                assert_eq!(
                    details.is_root,
                    nix::unistd::Uid::effective().is_root(),
                    "details root identity must come from Uid::effective rather than username"
                );
                assert!(
                    details.mount_points.iter().any(|mount| mount.path == "/"),
                    "the filesystem root should be present in mount details"
                );
                assert!(
                    details.mount_points.iter().all(|mount| {
                        mount.available_bytes.zip(mount.total_bytes).is_none_or(
                            |(available_bytes, total_bytes)| available_bytes <= total_bytes,
                        )
                    }),
                    "available mount capacity should not exceed total capacity"
                );
                assert!(
                    details.mount_points.iter().all(MountPoint::is_visible),
                    "virtual device, process, and LXC filesystems should stay hidden"
                );
                // Binary identity must match the same bake used by the server home page.
                assert_eq!(details.binary, current_binary_identity());
            }
            _ => panic!("Expected GetAgentDetails result"),
        }
    }
}
