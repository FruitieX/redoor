//! Agent-side command implementations, grouped by domain, behind one dispatch table.
//!
//! [`RunCommand`] is the only place that maps a wire command to its implementation; the runtime
//! dispatcher handles admission, cancellation, logging and the response around it.

pub(crate) mod content_grep;
pub(crate) mod ctx;
pub(crate) mod file_search;
mod fs;
mod git;
mod host;
mod metadata;

use crate::transfers::{
    download, edit::EditSession, raw_upload::RawFileSink, tar_upload::TarDirectorySink,
    upload::UploadDriver,
};
use ctx::{CommandCtx, Reply};
use redoor_protocol::wire::{
    command::{Command, CommandResult},
    policy::UploadKind,
};
use std::future::Future;

/// Executes one wire command with its dispatcher-provided context.
pub(crate) trait RunCommand {
    /// Runs the command to completion and reports how its outcome is delivered.
    fn run(self, ctx: CommandCtx) -> impl Future<Output = Reply> + Send;
}

impl RunCommand for Command {
    /// The single exhaustive mapping from wire commands to their domain implementations.
    async fn run(self, ctx: CommandCtx) -> Reply {
        match self {
            Command::Exec { request } => match ctx.into_output() {
                Ok(output) => crate::exec::run(request, output.stream, output.registration).await,
                Err(error) => CommandResult::from(error).into(),
            },
            Command::Ls { path } => fs::ls(path).await.into(),
            Command::FileSearch {
                path,
                query,
                timeout_seconds,
                include_hidden,
                respect_gitignore,
                case_sensitivity,
            } => file_search::execute(
                file_search::FileSearchRequest {
                    path,
                    query,
                    timeout_seconds,
                    include_hidden,
                    respect_gitignore,
                    case_sensitivity,
                },
                &ctx.cancel,
            )
            .await
            .into(),
            Command::ContentGrep {
                path,
                query,
                timeout_seconds,
                include_hidden,
                respect_gitignore,
                fixed_string,
                case_sensitivity,
                before_context,
                after_context,
            } => content_grep::execute(
                content_grep::ContentGrepRequest {
                    path,
                    query,
                    timeout_seconds,
                    include_hidden,
                    respect_gitignore,
                    fixed_string,
                    case_sensitivity,
                    before_context,
                    after_context,
                },
                &ctx.cancel,
                ctx.services.grep_slots.clone(),
            )
            .await
            .into(),
            Command::RawDownload {
                path,
                range_start,
                range_end,
            } => download::download_file(path, range_start, range_end, ctx).await,
            Command::TarDownload { path, include_root } => {
                download::download_directory(path, include_root, ctx).await
            }
            Command::RawUpload {
                path,
                on_existing,
                ownership,
            } => {
                let prepared = RawFileSink::prepare(
                    ctx.responder.request_id(),
                    path.clone(),
                    on_existing,
                    ownership,
                )
                .await;
                UploadDriver::new(UploadKind::RawUpload, path, ctx)
                    .run(prepared)
                    .await
            }
            Command::EditFile { path } => {
                let prepared = EditSession::open(ctx.responder.request_id(), path.clone()).await;
                UploadDriver::new(UploadKind::EditFile, path, ctx)
                    .run(prepared)
                    .await
            }
            Command::TarUpload { path, on_existing } => {
                let prepared = TarDirectorySink::prepare(
                    ctx.responder.request_id(),
                    path.clone(),
                    on_existing,
                )
                .await;
                UploadDriver::new(UploadKind::TarUpload, path, ctx)
                    .run(prepared)
                    .await
            }
            Command::LocalCopyFile {
                source_path,
                dest_path,
                on_existing,
            } => ctx
                .copy_context()
                .copy_command(source_path, dest_path, on_existing, false)
                .await
                .into(),
            Command::LocalCopyDirectory {
                source_path,
                dest_path,
                on_existing,
            } => ctx
                .copy_context()
                .copy_command(source_path, dest_path, on_existing, true)
                .await
                .into(),
            Command::LocalMove {
                source_path,
                dest_path,
                source_is_directory,
                expected_identity,
                on_existing,
            } => ctx
                .copy_context()
                .move_command(
                    source_path,
                    dest_path,
                    source_is_directory,
                    expected_identity,
                    on_existing,
                )
                .await
                .into(),
            Command::RawDelete { path } => fs::raw_delete(path).await.into(),
            Command::Trash { path } => ctx.services.trash.move_to_trash(path).await.into(),
            Command::ListTrash => ctx.services.trash.list().await.into(),
            Command::EmptyTrash => ctx.services.trash.empty().await.into(),
            Command::RestoreTrash {
                location_id,
                item_id,
                destination_path,
            } => ctx
                .services
                .trash
                .restore(&location_id, &item_id, destination_path, ctx.copy_context())
                .await
                .into(),
            Command::CreateDirectory { path, ownership } => {
                fs::create_directory(path, ownership).await.into()
            }
            Command::RenamePath { dir, old, new } => fs::rename_path(dir, old, new).await.into(),
            Command::ChownPath { path, owner, group } => {
                fs::chown_path(path, owner, group).await.into()
            }
            Command::ChmodPath { path, permissions } => {
                fs::chmod_path(path, permissions).await.into()
            }
            Command::ListAccounts => host::list_accounts().await.into(),
            Command::Metadata { path } => metadata::execute(path).await.into(),
            Command::DirectorySize {
                path,
                timeout_seconds,
            } => fs::directory_size(path, timeout_seconds, &ctx.cancel)
                .await
                .into(),
            Command::GitContext { path } => git::context(path).await.into(),
            Command::GitStatus { path } => git::status(path).await.into(),
            Command::GitDiff { files, mode } => git::diff(files, mode).await.into(),
            Command::MoveMetadata { path } => fs::move_metadata(path).await.into(),
            Command::DeleteMoveSource {
                path,
                expected_identity,
            } => fs::delete_move_source(path, expected_identity).await.into(),
            Command::OpenPath { path } => host::open_path(path).await.into(),
            Command::Echo { request } => host::echo(request).await.into(),
            Command::AgentInfo => host::agent_info().await.into(),
            Command::GetAgentDetails => host::get_agent_details(&ctx.services.loaded_config_path)
                .await
                .into(),
            Command::GetLoggingLevel => {
                CommandResult::LoggingLevel(redoor_core::logging::level()).into()
            }
            Command::SetLoggingLevel { level } => host::set_logging_level(level).into(),
            Command::Restart => CommandResult::Restart.into(),
            Command::SelfExec { path } => host::self_exec(path).into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RunCommand;
    use super::ctx::{CommandCtx, Reply};
    use redoor_core::identity::current_binary_identity;
    use redoor_core::test_support::TempDir;
    use redoor_protocol::{
        api::{
            agents::{EchoRequest, MountPoint},
            files::CreationOwnershipOptions,
        },
        ids::RequestId,
        wire::command::{Command, CommandErrorKind, CommandResult},
    };

    /// Runs one control-lane command through the real dispatch table without a connection.
    async fn run(command: Command) -> CommandResult {
        let (ctx, _frames) = CommandCtx::detached(RequestId::new(1));
        match command.run(ctx).await {
            Reply::Result(result) => result,
            Reply::Stream { .. } => panic!("control commands answer directly"),
        }
    }

    #[tokio::test]
    async fn test_ls_command() {
        let result = run(Command::Ls { path: None }).await;

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
        let path = "nonexistent-directory-for-ls-context";
        let result = run(Command::Ls {
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
    async fn test_raw_delete_command() {
        let temp = TempDir::create();
        let temp_path = temp.path().join("delete-test.txt");

        tokio::fs::write(&temp_path, "delete me")
            .await
            .expect("temporary file should be created");

        let result = run(Command::RawDelete {
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

        let result = run(Command::RawDelete {
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
        let result = run(Command::Echo {
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
        let temp = TempDir::create();
        let nested_dir = temp.path().join("nested").join("child");

        let result = run(Command::CreateDirectory {
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
        let temp = TempDir::create();
        let source_path = temp.path().join("before.txt");
        let dest_path = temp.path().join("after.txt");
        tokio::fs::write(&source_path, "rename me")
            .await
            .expect("source file should be created");

        let result = run(Command::RenamePath {
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

        let result = run(Command::ChmodPath {
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
        let temp = TempDir::create();
        let path = temp.path().join("owned.txt");
        tokio::fs::write(&path, "owned")
            .await
            .expect("chown fixture should be created");

        let result = run(Command::ChownPath {
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
        let result = run(Command::GetAgentDetails).await;

        match result {
            CommandResult::GetAgentDetails(details) => {
                assert!(details.pid > 0, "PID should be positive");
                assert!(!details.cwd.is_empty(), "CWD should not be empty");
                // Detached test services record no loaded config, so the path stays empty.
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
