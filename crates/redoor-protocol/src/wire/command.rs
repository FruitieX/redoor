//! Commands the server sends to agents and the results agents return.

use crate::api::agents::{AgentAccountsResponse, AgentDetailsResponse, EchoRequest};
use crate::api::files::{
    ChmodPathResponse, ChownPathResponse, CreationOwnershipOptions, DirectorySizeResponse, LsEntry,
    MetadataResponse,
};
use crate::api::git::{GitContextResponse, GitDiffMode, GitDiffResponse, GitStatusResponse};
use crate::api::search::{
    CaseSensitivity, ContentGrepResponse, FileSearchResponse, default_file_search_timeout_seconds,
    default_true,
};
use crate::api::transfers::CopyExistingMode;
use crate::api::trash::TrashListResponse;
use crate::wire::exec::{ExecEvent, ExecRequest};
use crate::wire::log::Level;
use serde::{Deserialize, Serialize};

/// Every operation the server can ask an agent to perform; one enum keeps the wire contract
/// and the agent's dispatch match exhaustive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Command {
    /// Streams non-shell process output through the existing bounded data lane.
    Exec {
        request: ExecRequest,
    },
    Ls {
        path: Option<String>,
    },
    FileSearch {
        path: String,
        query: String,
        /// Keeps the deadline agent-local so traversal stops even if the REST caller disconnects.
        #[serde(default = "default_file_search_timeout_seconds")]
        timeout_seconds: u64,
        /// Hidden directories are opt-in because they often contain large caches and metadata trees.
        #[serde(default)]
        include_hidden: bool,
        /// Git ignore rules are enabled by default so searches match repository expectations.
        #[serde(default = "default_true")]
        respect_gitignore: bool,
        /// Smart case keeps lowercase searches forgiving while uppercase remains intentional.
        #[serde(default)]
        case_sensitivity: CaseSensitivity,
    },
    /// Searches physical file lines without making the server read remote content.
    ContentGrep {
        path: String,
        query: String,
        /// Keeps the whole operation, including exclusive-slot waiting, agent-local and bounded.
        #[serde(default = "default_file_search_timeout_seconds")]
        timeout_seconds: u64,
        /// Hidden directories are opt-in to avoid unexpectedly scanning cache trees.
        #[serde(default)]
        include_hidden: bool,
        /// Repository ignore rules are enabled by default to match recursive filename search.
        #[serde(default = "default_true")]
        respect_gitignore: bool,
        /// Literal matching is opt-in so existing callers keep regex semantics.
        #[serde(default)]
        fixed_string: bool,
        /// Smart case keeps lowercase searches forgiving while uppercase remains intentional.
        #[serde(default)]
        case_sensitivity: CaseSensitivity,
        /// Preceding physical lines are opt-in and bounded to keep each match response small.
        #[serde(default)]
        before_context: u64,
        /// Following physical lines are opt-in and bounded to keep each match response small.
        #[serde(default)]
        after_context: u64,
    },
    RawDownload {
        path: String,
        range_start: Option<u64>,
        range_end: Option<u64>,
    },
    TarDownload {
        path: String,
        /// Includes the source directory itself for archives intended for user extraction.
        #[serde(default)]
        include_root: bool,
    },
    RawUpload {
        path: String,
        /// Controls replacement when the destination already exists.
        /// Defaults to override so direct HTTP raw uploads keep replacing files.
        #[serde(default = "CopyExistingMode::override_mode")]
        on_existing: CopyExistingMode,
        /// Applies only to the uploaded destination entry, not its parent directories.
        #[serde(default)]
        ownership: CreationOwnershipOptions,
    },
    /// Rewrites one existing regular-file inode after staging the complete body.
    EditFile {
        path: String,
    },
    TarUpload {
        path: String,
        /// Controls replacement when the destination already exists.
        /// Defaults to error so directory placement stays non-destructive unless copy asks otherwise.
        #[serde(default)]
        on_existing: CopyExistingMode,
    },
    LocalCopyFile {
        source_path: String,
        dest_path: String,
        /// Controls replacement when the destination already exists.
        #[serde(default)]
        on_existing: CopyExistingMode,
    },
    LocalCopyDirectory {
        source_path: String,
        dest_path: String,
        /// Controls replacement when the destination already exists.
        #[serde(default)]
        on_existing: CopyExistingMode,
    },
    /// Moves one file or directory locally, preferring an atomic filesystem rename.
    LocalMove {
        source_path: String,
        dest_path: String,
        source_is_directory: bool,
        /// Prevents either rename or fallback cleanup from acting on a replacement source.
        expected_identity: MoveSourceIdentity,
        /// Controls replacement when the destination path already exists.
        #[serde(default)]
        on_existing: CopyExistingMode,
    },
    RawDelete {
        path: String,
    },
    /// Moves one filesystem entry into the platform trash without copy/delete fallback.
    Trash {
        path: String,
    },
    /// Returns the platform-neutral inventory discovered by the agent at command time.
    ListTrash,
    /// Permanently removes every payload and metadata entry from discovered trash locations.
    EmptyTrash,
    /// Restores one opaque inventory item to an explicit user-selected destination.
    RestoreTrash {
        location_id: String,
        item_id: String,
        destination_path: String,
    },
    CreateDirectory {
        path: String,
        /// Applies only to the requested directory after recursive parent creation.
        #[serde(default)]
        ownership: CreationOwnershipOptions,
    },
    RenamePath {
        dir: String,
        old: String,
        new: String,
    },
    /// Changes owner and/or group of an existing path, following the same inode `ls` displays.
    ChownPath {
        path: String,
        owner: Option<String>,
        group: Option<String>,
    },
    /// Replaces the ordinary rwx bits of an existing path without clearing setuid/setgid/sticky.
    ChmodPath {
        path: String,
        permissions: u32,
    },
    /// Enumerates host user and group names for root-only ownership editors.
    ListAccounts,
    Metadata {
        path: String,
    },
    /// Calculates recursive regular-file bytes without delaying directory navigation.
    DirectorySize {
        path: String,
        /// Keeps expensive walks bounded on the agent even if the REST request disconnects.
        timeout_seconds: u64,
    },
    /// Discovers repository membership and classification for one browser path.
    GitContext {
        path: String,
    },
    /// Lists bounded repository changes below one browser directory.
    GitStatus {
        path: String,
    },
    /// Compares ordered files with HEAD using the selected Git source.
    GitDiff {
        files: Vec<String>,
        mode: GitDiffMode,
    },
    /// Reads the stable filesystem identity needed to delete only the source that was copied.
    MoveMetadata {
        path: String,
    },
    /// Removes a move source only while it still has the identity captured before copying.
    DeleteMoveSource {
        path: String,
        expected_identity: MoveSourceIdentity,
    },
    OpenPath {
        path: String,
    },
    Echo {
        request: EchoRequest,
    },
    AgentInfo,
    GetAgentDetails,
    /// Reads the process-wide threshold from the agent that owns this control socket.
    GetLoggingLevel,
    /// Changes logging admission for subsequent events in the connected agent process.
    SetLoggingLevel {
        level: Level,
    },
    Restart,
    SelfExec {
        path: String,
    },
}

impl Command {
    /// Short operator-facing label without large or sensitive payloads.
    pub fn summary(&self) -> String {
        match self {
            Self::Ls { path } => match path {
                Some(path) => format!("Ls path={path}"),
                None => "Ls path=.".to_string(),
            },
            Self::FileSearch {
                path,
                query,
                timeout_seconds,
                include_hidden,
                respect_gitignore,
                case_sensitivity,
            } => {
                format!(
                    "FileSearch path={path} query={query} timeout={timeout_seconds}s include_hidden={include_hidden} respect_gitignore={respect_gitignore} case_sensitivity={case_sensitivity:?}"
                )
            }
            Self::ContentGrep {
                path,
                query,
                timeout_seconds,
                include_hidden,
                respect_gitignore,
                fixed_string,
                case_sensitivity,
                before_context,
                after_context,
            } => format!(
                "ContentGrep path={path} query={query} timeout={timeout_seconds}s include_hidden={include_hidden} respect_gitignore={respect_gitignore} fixed_string={fixed_string} case_sensitivity={case_sensitivity:?} before_context={before_context} after_context={after_context}"
            ),
            Self::RawDownload {
                path,
                range_start,
                range_end,
            } => match (range_start, range_end) {
                (Some(start), Some(end)) => {
                    format!("RawDownload path={path} range={start}-{end}")
                }
                (Some(start), None) => format!("RawDownload path={path} range_start={start}"),
                (None, Some(end)) => format!("RawDownload path={path} range_end={end}"),
                (None, None) => format!("RawDownload path={path}"),
            },
            Self::TarDownload { path, include_root } => {
                format!("TarDownload path={path} include_root={include_root}")
            }
            Self::Exec { .. } => "Exec".to_owned(),
            Self::RawUpload {
                path, on_existing, ..
            } => {
                format!("RawUpload path={path} on_existing={on_existing:?}")
            }
            Self::EditFile { path } => format!("EditFile path={path}"),
            Self::TarUpload { path, on_existing } => {
                format!("TarUpload path={path} on_existing={on_existing:?}")
            }
            Self::LocalCopyFile {
                source_path,
                dest_path,
                on_existing,
            } => format!(
                "LocalCopyFile source={source_path} dest={dest_path} on_existing={on_existing:?}"
            ),
            Self::LocalCopyDirectory {
                source_path,
                dest_path,
                on_existing,
            } => format!(
                "LocalCopyDirectory source={source_path} dest={dest_path} on_existing={on_existing:?}"
            ),
            Self::LocalMove {
                source_path,
                dest_path,
                source_is_directory,
                on_existing,
                ..
            } => format!(
                "LocalMove source={source_path} dest={dest_path} directory={source_is_directory} on_existing={on_existing:?}"
            ),
            Self::RawDelete { path } => format!("RawDelete path={path}"),
            Self::Trash { path } => format!("Trash path={path}"),
            Self::ListTrash => "ListTrash".to_string(),
            Self::EmptyTrash => "EmptyTrash".to_string(),
            Self::RestoreTrash {
                location_id,
                item_id,
                destination_path,
            } => format!(
                "RestoreTrash location={location_id} item={item_id} destination={destination_path}"
            ),
            Self::CreateDirectory { path, .. } => format!("CreateDirectory path={path}"),
            Self::RenamePath { dir, old, new } => {
                format!("RenamePath dir={dir} old={old} new={new}")
            }
            Self::ChownPath { path, owner, group } => {
                format!("ChownPath path={path} owner={owner:?} group={group:?}")
            }
            Self::ChmodPath { path, permissions } => {
                format!("ChmodPath path={path} permissions={permissions:#o}")
            }
            Self::ListAccounts => "ListAccounts".to_string(),
            Self::Metadata { path } => format!("Metadata path={path}"),
            Self::DirectorySize {
                path,
                timeout_seconds,
            } => format!("DirectorySize path={path} timeout={timeout_seconds}s"),
            Self::GitContext { path } => format!("GitContext path={path}"),
            Self::GitStatus { path } => format!("GitStatus path={path}"),
            Self::GitDiff { files, mode } => {
                format!("GitDiff files={} mode={mode:?}", files.len())
            }
            Self::MoveMetadata { path } => format!("MoveMetadata path={path}"),
            Self::DeleteMoveSource { path, .. } => format!("DeleteMoveSource path={path}"),
            Self::OpenPath { path } => format!("OpenPath path={path}"),
            // Echo bodies can be large or sensitive, so only the command name is logged.
            Self::Echo { .. } => "Echo".to_string(),
            Self::AgentInfo => "AgentInfo".to_string(),
            Self::GetAgentDetails => "GetAgentDetails".to_string(),
            Self::GetLoggingLevel => "GetLoggingLevel".to_string(),
            Self::SetLoggingLevel { level } => format!("SetLoggingLevel level={level}"),
            Self::Restart => "Restart".to_string(),
            Self::SelfExec { path } => format!("SelfExec path={path}"),
        }
    }
}

/// Carries directory metadata with its entries so clients can show either a list or details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsDirectoryResult {
    pub files: Vec<LsEntry>,
    pub path: String,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub uid: u32,
    pub gid: u32,
    pub permissions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Carries file metadata from an agent without requiring the server to inspect the remote filesystem.
pub struct LsFileResult {
    pub size: u64,
    pub path: String,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub uid: u32,
    pub gid: u32,
    pub permissions: u32,
}

/// Identifies one filesystem object strongly enough to refuse deletion after path replacement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MoveSourceIdentity {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub modified_seconds: i64,
    pub modified_nanoseconds: i64,
    pub is_directory: bool,
}

impl MoveSourceIdentity {
    /// Captures the identity from one metadata read so every move, trash restore and conditional
    /// source deletion compares the same fields.
    #[cfg(unix)]
    pub fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;

        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            is_directory: metadata.is_dir(),
        }
    }
}

/// Returns move-specific metadata without exposing filesystem identity through the public API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveMetadataResult {
    pub file_size: u64,
    pub is_file: bool,
    pub is_dir: bool,
    pub identity: MoveSourceIdentity,
}

/// Agent's echo reply, used to verify the command round trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EchoResult {
    pub message: String,
}

/// Lightweight process and host load snapshot for agent status views.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfoResult {
    pub pid: u32,
    pub cwd: String,
    pub load_average: (f64, f64, f64),
    pub system_uptime: u64,
}

/// Classifies command failures so transports do not depend on OS-specific text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandErrorKind {
    NotFound,
    PermissionDenied,
    NotADirectory,
    IsDirectory,
    AlreadyExists,
    Conflict,
    InvalidInput,
    ServiceUnavailable,
    /// The server canceled the request before it finished.
    Canceled,
    Internal,
}

impl CommandErrorKind {
    /// Converts one I/O failure into a stable command error kind.
    pub fn from_io_error(error: &std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound,
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            std::io::ErrorKind::NotADirectory => Self::NotADirectory,
            std::io::ErrorKind::IsADirectory => Self::IsDirectory,
            std::io::ErrorKind::AlreadyExists => Self::AlreadyExists,
            std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData => {
                Self::InvalidInput
            }
            _ => Self::Internal,
        }
    }
}

/// Terminal result of one `Command`, correlated by request id; streaming commands use it
/// only to report completion since their data travels as binary frames.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CommandResult {
    /// Carries an agent-reaped execution result independently of saturated payload transport.
    Exec {
        event: ExecEvent,
    },
    LsDirectory(LsDirectoryResult),
    LsFile(LsFileResult),
    FileSearch(FileSearchResponse),
    ContentGrep(ContentGrepResponse),
    RawDownload {
        path: String,
    },
    TarDownload {
        path: String,
    },
    RawUpload,
    EditFile,
    TarUpload,
    LocalCopyFile,
    LocalCopyDirectory,
    /// Carries whether a filesystem rename published the destination so copy speeds stay hidden.
    LocalMove {
        atomic: bool,
    },
    RawDelete,
    Trash,
    TrashList(TrashListResponse),
    EmptyTrash {
        deleted_items: u64,
    },
    RestoreTrash {
        path: String,
    },
    CreateDirectory,
    RenamePath,
    ChownPath(ChownPathResponse),
    ChmodPath(ChmodPathResponse),
    ListAccounts(AgentAccountsResponse),
    Metadata(MetadataResponse),
    DirectorySize(DirectorySizeResponse),
    GitContext(GitContextResponse),
    GitStatus(GitStatusResponse),
    GitDiff(GitDiffResponse),
    MoveMetadata(MoveMetadataResult),
    OpenPath,
    Echo(EchoResult),
    AgentInfo(AgentInfoResult),
    GetAgentDetails(Box<AgentDetailsResponse>),
    LoggingLevel(Level),
    Restart,
    SelfExec {
        path: String,
    },
    Error {
        kind: CommandErrorKind,
        message: String,
    },
}

impl CommandResult {
    /// Short operator-facing outcome without dumping large command payloads.
    pub fn summary(&self) -> String {
        match self {
            Self::LsDirectory(_) => "ok LsDirectory".to_string(),
            Self::Exec { event } => format!("ok Exec {event:?}"),
            Self::LsFile(_) => "ok LsFile".to_string(),
            Self::FileSearch(result) => format!(
                "ok FileSearch results={} timed_out={}",
                result.results.len(),
                result.timed_out
            ),
            Self::ContentGrep(result) => format!(
                "ok ContentGrep results={} timed_out={} cancelled={} truncated={}",
                result.results.len(),
                result.timed_out,
                result.cancelled,
                result.truncated
            ),
            Self::RawDownload { path } => format!("ok RawDownload path={path}"),
            Self::TarDownload { path } => format!("ok TarDownload path={path}"),
            Self::RawUpload => "ok RawUpload".to_string(),
            Self::EditFile => "ok EditFile".to_string(),
            Self::TarUpload => "ok TarUpload".to_string(),
            Self::LocalCopyFile => "ok LocalCopyFile".to_string(),
            Self::LocalCopyDirectory => "ok LocalCopyDirectory".to_string(),
            Self::LocalMove { atomic } => format!("ok LocalMove atomic={atomic}"),
            Self::RawDelete => "ok RawDelete".to_string(),
            Self::Trash => "ok Trash".to_string(),
            Self::TrashList(result) => format!("ok TrashList locations={}", result.locations.len()),
            Self::EmptyTrash { deleted_items } => {
                format!("ok EmptyTrash deleted_items={deleted_items}")
            }
            Self::RestoreTrash { path } => format!("ok RestoreTrash path={path}"),
            Self::CreateDirectory => "ok CreateDirectory".to_string(),
            Self::RenamePath => "ok RenamePath".to_string(),
            Self::ChownPath(result) => {
                format!(
                    "ok ChownPath path={} uid={} gid={}",
                    result.path, result.uid, result.gid
                )
            }
            Self::ChmodPath(result) => {
                format!(
                    "ok ChmodPath path={} permissions={:#o}",
                    result.path, result.permissions
                )
            }
            Self::ListAccounts(result) => format!(
                "ok ListAccounts users={} groups={} truncated={}",
                result.users.len(),
                result.groups.len(),
                result.truncated
            ),
            Self::Metadata(_) => "ok Metadata".to_string(),
            Self::DirectorySize(result) => format!("ok DirectorySize size={}", result.size),
            Self::GitContext(GitContextResponse::OutsideWorktree) => {
                "ok GitContext status=outside_worktree".to_string()
            }
            Self::GitContext(GitContextResponse::InsideWorktree { tracking_state, .. }) => {
                format!("ok GitContext status=inside_worktree tracking={tracking_state:?}")
            }
            Self::GitStatus(result) => format!(
                "ok GitStatus entries={} truncated={} omitted_non_utf8={}",
                result.entries.len(),
                result.truncated,
                result.omitted_non_utf8_entries
            ),
            // Patch bodies must never enter logs because they may be large or sensitive.
            Self::GitDiff(result) => format!(
                "ok GitDiff files={} results={}",
                result.diffs.len(),
                result
                    .diffs
                    .iter()
                    .map(|diff| diff.result.summary())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Self::MoveMetadata(_) => "ok MoveMetadata".to_string(),
            Self::OpenPath => "ok OpenPath".to_string(),
            Self::Echo(_) => "ok Echo".to_string(),
            Self::AgentInfo(_) => "ok AgentInfo".to_string(),
            Self::GetAgentDetails(_) => "ok GetAgentDetails".to_string(),
            Self::LoggingLevel(level) => format!("ok LoggingLevel level={level}"),
            Self::Restart => "ok Restart".to_string(),
            Self::SelfExec { path } => format!("ok SelfExec path={path}"),
            Self::Error { kind, message } => {
                format!("error kind={kind:?} message={message}")
            }
        }
    }

    /// Builds one structured command failure when the caller already knows the error kind.
    pub fn error(kind: CommandErrorKind, message: impl Into<String>) -> Self {
        Self::Error {
            kind,
            message: message.into(),
        }
    }

    /// Builds one structured command failure from an I/O error.
    pub fn io_error(context: &str, error: std::io::Error) -> Self {
        let kind = CommandErrorKind::from_io_error(&error);
        Self::Error {
            kind,
            message: format!("{}: {}", context, error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tar_download_defaults_to_excluding_root() {
        let command: Command = serde_json::from_str(r#"{"type":"TarDownload","path":"test-dir"}"#)
            .expect("legacy tar download command should deserialize");

        match command {
            Command::TarDownload { include_root, .. } => {
                // Missing fields from older servers must retain the flat archive used by copies.
                assert!(!include_root);
            }
            _ => panic!("Expected TarDownload"),
        }
    }

    #[test]
    fn older_content_grep_response_defaults_context_capability() {
        let result: CommandResult = serde_json::from_str(
            r#"{"type":"ContentGrep","results":[{"path":"/tmp/file","line_number":1,"line":"match","line_truncated":false}],"timed_out":false,"cancelled":false,"truncated":false,"omitted_long_lines":0,"duration_ms":1}"#,
        )
        .expect("older grep response should deserialize");

        let CommandResult::ContentGrep(response) = result else {
            panic!("Expected ContentGrep");
        };
        // A missing capability identifies a connected agent that predates context support.
        assert!(!response.context_supported);
        // Defaulted arrays keep ordinary zero-context grep usable during a rolling upgrade.
        assert!(response.results[0].before_context.is_empty());
        assert!(response.results[0].after_context.is_empty());
    }
}
