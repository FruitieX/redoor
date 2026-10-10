//! Execution policy for every wire command.
//!
//! The agent decides admission, cancellation and task ownership from this table and the
//! server derives upload semantics from it, so a new command is classified in exactly one
//! exhaustive match instead of several hand-maintained `matches!` lists that can drift.

use crate::api::transfers::TransferDirection;
use crate::wire::command::{Command, CommandResult};

/// Socket lanes a command occupies while it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandLane {
    /// Request and response are control frames only.
    Control,
    /// The agent streams output bytes to the server on the request's stream.
    Output,
    /// The server streams input bytes to the agent on the request's stream.
    Upload(UploadKind),
}

/// Distinguishes inbound byte streams that share transport but not publication semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadKind {
    /// Replaces or creates one file through a sibling staging file.
    RawUpload,
    /// Extracts a tar stream into a sibling staging directory before placement.
    TarUpload,
    /// Rewrites an existing inode in place after staging the full body.
    EditFile,
}

impl UploadKind {
    /// Selects the direction exposed through transfer progress APIs.
    pub fn direction(self) -> TransferDirection {
        match self {
            Self::RawUpload | Self::TarUpload => TransferDirection::Upload,
            Self::EditFile => TransferDirection::Edit,
        }
    }

    /// Ensures one stream cannot be completed by another write operation's response.
    pub fn completion_matches(self, result: &CommandResult) -> bool {
        matches!(
            (self, result),
            (Self::RawUpload, CommandResult::RawUpload)
                | (Self::TarUpload, CommandResult::TarUpload)
                | (Self::EditFile, CommandResult::EditFile)
        )
    }
}

/// How a running command reacts to explicit cancellation and connection loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelScope {
    /// Dropped when its control connection ends; there is no explicit cancel message for it.
    Connection,
    /// Registered by request id so `cancel` and connection loss stop it
    /// cooperatively, letting the command remove its staging output before it reports.
    Request,
    /// Runs to completion even when the connection ends because interrupting a half-applied
    /// mutation (trash rename, purge) would be worse than delaying teardown.
    Uninterruptible,
}

/// Admission and lifecycle rules for one command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandPolicy {
    /// Whether the command streams bytes, and in which direction.
    pub lane: CommandLane,
    /// Which cancellation sources stop the command and whether it may be dropped.
    pub cancel_scope: CancelScope,
    /// Serialized with every other exclusive command because they mutate shared agent-owned
    /// state (the trash inventory) that concurrent writers could corrupt.
    pub exclusive: bool,
}

impl CommandPolicy {
    /// Ordinary request/response command that can be dropped on disconnect.
    const CONTROL: Self = Self {
        lane: CommandLane::Control,
        cancel_scope: CancelScope::Connection,
        exclusive: false,
    };
    /// Same-agent work that owns staging output and must clean it up when canceled.
    const LOCAL_TRANSFER: Self = Self {
        lane: CommandLane::Control,
        cancel_scope: CancelScope::Request,
        exclusive: false,
    };
    /// Agent-to-server byte stream.
    const OUTPUT: Self = Self {
        lane: CommandLane::Output,
        cancel_scope: CancelScope::Request,
        exclusive: false,
    };

    /// Server-to-agent byte stream with the given publication semantics.
    const fn upload(kind: UploadKind) -> Self {
        Self {
            lane: CommandLane::Upload(kind),
            cancel_scope: CancelScope::Request,
            exclusive: false,
        }
    }

    /// Trash mutation that must not interleave with other trash mutations.
    const fn trash_mutation(cancel_scope: CancelScope) -> Self {
        Self {
            lane: CommandLane::Control,
            cancel_scope,
            exclusive: true,
        }
    }

    /// Whether the command carries a byte stream in either direction.
    pub fn streams_bytes(&self) -> bool {
        !matches!(self.lane, CommandLane::Control)
    }

    /// Upload semantics when the command receives a server-to-agent byte stream.
    pub fn upload_kind(&self) -> Option<UploadKind> {
        match self.lane {
            CommandLane::Upload(kind) => Some(kind),
            CommandLane::Control | CommandLane::Output => None,
        }
    }
}

impl Command {
    /// Returns the single authoritative execution policy for this command.
    pub fn policy(&self) -> CommandPolicy {
        match self {
            Self::Exec { .. } | Self::RawDownload { .. } | Self::TarDownload { .. } => {
                CommandPolicy::OUTPUT
            }
            Self::RawUpload { .. } => CommandPolicy::upload(UploadKind::RawUpload),
            Self::TarUpload { .. } => CommandPolicy::upload(UploadKind::TarUpload),
            Self::EditFile { .. } => CommandPolicy::upload(UploadKind::EditFile),
            Self::LocalCopyFile { .. }
            | Self::LocalCopyDirectory { .. }
            | Self::LocalMove { .. } => CommandPolicy::LOCAL_TRANSFER,
            Self::Trash { .. } | Self::EmptyTrash => {
                CommandPolicy::trash_mutation(CancelScope::Uninterruptible)
            }
            // Restore is a move that reports progress and owns staging, so it stays cancelable.
            Self::RestoreTrash { .. } => CommandPolicy::trash_mutation(CancelScope::Request),
            Self::Ls { .. }
            | Self::FileSearch { .. }
            | Self::ContentGrep { .. }
            | Self::RawDelete { .. }
            | Self::ListTrash
            | Self::CreateDirectory { .. }
            | Self::RenamePath { .. }
            | Self::ChownPath { .. }
            | Self::ChmodPath { .. }
            | Self::ListAccounts
            | Self::Metadata { .. }
            | Self::DirectorySize { .. }
            | Self::GitContext { .. }
            | Self::GitStatus { .. }
            | Self::GitDiff { .. }
            | Self::MoveMetadata { .. }
            | Self::DeleteMoveSource { .. }
            | Self::OpenPath { .. }
            | Self::Echo { .. }
            | Self::AgentInfo
            | Self::GetAgentDetails
            | Self::GetLoggingLevel
            | Self::SetLoggingLevel { .. }
            | Self::Restart
            | Self::SelfExec { .. } => CommandPolicy::CONTROL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::agents::EchoRequest;
    use crate::api::files::CreationOwnershipOptions;
    use crate::api::git::GitDiffMode;
    use crate::api::search::CaseSensitivity;
    use crate::api::transfers::CopyExistingMode;
    use crate::wire::command::MoveSourceIdentity;
    use crate::wire::exec::ExecRequest;
    use crate::wire::log::Level;

    /// Maps every variant to a dense index; adding a variant fails to compile here first, and
    /// the coverage assertion below then forces a matching sample.
    fn variant_index(command: &Command) -> usize {
        match command {
            Command::Exec { .. } => 0,
            Command::Ls { .. } => 1,
            Command::FileSearch { .. } => 2,
            Command::ContentGrep { .. } => 3,
            Command::RawDownload { .. } => 4,
            Command::TarDownload { .. } => 5,
            Command::RawUpload { .. } => 6,
            Command::EditFile { .. } => 7,
            Command::TarUpload { .. } => 8,
            Command::LocalCopyFile { .. } => 9,
            Command::LocalCopyDirectory { .. } => 10,
            Command::LocalMove { .. } => 11,
            Command::RawDelete { .. } => 12,
            Command::Trash { .. } => 13,
            Command::ListTrash => 14,
            Command::EmptyTrash => 15,
            Command::RestoreTrash { .. } => 16,
            Command::CreateDirectory { .. } => 17,
            Command::RenamePath { .. } => 18,
            Command::ChownPath { .. } => 19,
            Command::ChmodPath { .. } => 20,
            Command::ListAccounts => 21,
            Command::Metadata { .. } => 22,
            Command::DirectorySize { .. } => 23,
            Command::GitContext { .. } => 24,
            Command::GitStatus { .. } => 25,
            Command::GitDiff { .. } => 26,
            Command::MoveMetadata { .. } => 27,
            Command::DeleteMoveSource { .. } => 28,
            Command::OpenPath { .. } => 29,
            Command::Echo { .. } => 30,
            Command::AgentInfo => 31,
            Command::GetAgentDetails => 32,
            Command::GetLoggingLevel => 33,
            Command::SetLoggingLevel { .. } => 34,
            Command::Restart => 35,
            Command::SelfExec { .. } => 36,
        }
    }

    /// Number of variants covered by `variant_index`.
    const VARIANT_COUNT: usize = 37;

    /// Builds one instance of every command variant for table-wide policy checks.
    fn every_command() -> Vec<Command> {
        let path = || "/tmp/path".to_string();
        let identity = MoveSourceIdentity {
            device: 1,
            inode: 2,
            size: 3,
            modified_seconds: 4,
            modified_nanoseconds: 5,
            is_directory: false,
        };
        vec![
            Command::Exec {
                request: ExecRequest {
                    argv: vec!["true".to_string()],
                    cwd: None,
                    env: Default::default(),
                    timeout_ms: None,
                },
            },
            Command::Ls { path: None },
            Command::FileSearch {
                path: path(),
                query: "q".to_string(),
                timeout_seconds: 1,
                include_hidden: false,
                respect_gitignore: true,
                case_sensitivity: CaseSensitivity::Smart,
            },
            Command::ContentGrep {
                path: path(),
                query: "q".to_string(),
                timeout_seconds: 1,
                include_hidden: false,
                respect_gitignore: true,
                fixed_string: false,
                case_sensitivity: CaseSensitivity::Smart,
                before_context: 0,
                after_context: 0,
            },
            Command::RawDownload {
                path: path(),
                range_start: None,
                range_end: None,
            },
            Command::TarDownload {
                path: path(),
                include_root: false,
            },
            Command::RawUpload {
                path: path(),
                on_existing: CopyExistingMode::Override,
                ownership: CreationOwnershipOptions::default(),
            },
            Command::EditFile { path: path() },
            Command::TarUpload {
                path: path(),
                on_existing: CopyExistingMode::Error,
            },
            Command::LocalCopyFile {
                source_path: path(),
                dest_path: path(),
                on_existing: CopyExistingMode::Error,
            },
            Command::LocalCopyDirectory {
                source_path: path(),
                dest_path: path(),
                on_existing: CopyExistingMode::Error,
            },
            Command::LocalMove {
                source_path: path(),
                dest_path: path(),
                source_is_directory: false,
                expected_identity: identity.clone(),
                on_existing: CopyExistingMode::Error,
            },
            Command::RawDelete { path: path() },
            Command::Trash { path: path() },
            Command::ListTrash,
            Command::EmptyTrash,
            Command::RestoreTrash {
                location_id: "l".to_string(),
                item_id: "i".to_string(),
                destination_path: path(),
            },
            Command::CreateDirectory {
                path: path(),
                ownership: CreationOwnershipOptions::default(),
            },
            Command::RenamePath {
                dir: path(),
                old: "a".to_string(),
                new: "b".to_string(),
            },
            Command::ChownPath {
                path: path(),
                owner: None,
                group: None,
            },
            Command::ChmodPath {
                path: path(),
                permissions: 0o644,
            },
            Command::ListAccounts,
            Command::Metadata { path: path() },
            Command::DirectorySize {
                path: path(),
                timeout_seconds: 1,
            },
            Command::GitContext { path: path() },
            Command::GitStatus { path: path() },
            Command::GitDiff {
                files: Vec::new(),
                mode: GitDiffMode::Full,
            },
            Command::MoveMetadata { path: path() },
            Command::DeleteMoveSource {
                path: path(),
                expected_identity: identity,
            },
            Command::OpenPath { path: path() },
            Command::Echo {
                request: EchoRequest {
                    message: "m".to_string(),
                    random_sleep: false,
                },
            },
            Command::AgentInfo,
            Command::GetAgentDetails,
            Command::GetLoggingLevel,
            Command::SetLoggingLevel { level: Level::Info },
            Command::Restart,
            Command::SelfExec { path: path() },
        ]
    }

    /// Verifies the policy samples cover every command variant.
    #[test]
    fn samples_cover_every_command_variant() {
        let mut seen = vec![false; VARIANT_COUNT];
        for command in every_command() {
            seen[variant_index(&command)] = true;
        }
        // Every variant needs a sample so the consistency checks below see the whole policy table.
        assert!(
            seen.iter().all(|covered| *covered),
            "missing samples: {seen:?}"
        );
    }

    /// Verifies every transfer-lane command can be canceled by request id.
    #[test]
    fn streaming_commands_are_request_cancelable() {
        for command in every_command() {
            let policy = command.policy();
            if policy.streams_bytes() {
                // Byte streams own staging output or a server consumer, so cancel must reach them.
                assert_eq!(
                    policy.cancel_scope,
                    CancelScope::Request,
                    "{} streams bytes but cannot be canceled by request id",
                    command.summary()
                );
            }
        }
    }

    /// Verifies exclusive commands are never abortable.
    #[test]
    fn exclusive_commands_are_never_dropped_mid_mutation() {
        for command in every_command() {
            let policy = command.policy();
            if policy.exclusive {
                // Dropping a future that holds the exclusive slot could abandon a half-applied mutation.
                assert_ne!(
                    policy.cancel_scope,
                    CancelScope::Connection,
                    "{} is exclusive but abortable",
                    command.summary()
                );
            }
        }
    }

    /// Verifies upload kinds match their commands and results.
    #[test]
    fn upload_lane_matches_the_command_and_its_success_result() {
        let expected = [
            (6, UploadKind::RawUpload, CommandResult::RawUpload),
            (7, UploadKind::EditFile, CommandResult::EditFile),
            (8, UploadKind::TarUpload, CommandResult::TarUpload),
        ];
        let uploads: Vec<_> = every_command()
            .into_iter()
            .filter_map(|command| {
                command
                    .policy()
                    .upload_kind()
                    .map(|kind| (variant_index(&command), kind))
            })
            .collect();
        // Exactly the three upload commands may announce transfer readiness and receive chunks.
        assert_eq!(uploads.len(), expected.len());
        for (index, kind, success) in expected {
            // The server routes chunks and completion by this kind, so it must match the command.
            assert!(uploads.contains(&(index, kind)));
            // Only the command's own success result may complete its upload stream.
            assert!(kind.completion_matches(&success));
        }
        // Edits keep their own progress direction so the UI can distinguish inode rewrites.
        assert!(matches!(
            UploadKind::EditFile.direction(),
            TransferDirection::Edit
        ));
    }
}
