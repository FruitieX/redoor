//! Commands the server sends to agents and the shared types their responses use.

use crate::api::files::LsEntry;
use crate::wire::request;
use serde::{Deserialize, Serialize};

/// Every operation the server can ask an agent to perform; one enum keeps the wire contract
/// and the agent's dispatch match exhaustive. Each variant's payload is the request struct
/// that also names the command's response type (see [`request::AgentRequest`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Command {
    Exec(request::Exec),
    Ls(request::Ls),
    FileSearch(request::FileSearch),
    ContentGrep(request::ContentGrep),
    RawDownload(request::RawDownload),
    TarDownload(request::TarDownload),
    RawUpload(request::RawUpload),
    EditFile(request::EditFile),
    TarUpload(request::TarUpload),
    LocalCopyFile(request::LocalCopyFile),
    LocalCopyDirectory(request::LocalCopyDirectory),
    LocalMove(request::LocalMove),
    RawDelete(request::RawDelete),
    Trash(request::Trash),
    ListTrash(request::ListTrash),
    EmptyTrash(request::EmptyTrash),
    RestoreTrash(request::RestoreTrash),
    CreateDirectory(request::CreateDirectory),
    RenamePath(request::RenamePath),
    ChownPath(request::ChownPath),
    ChmodPath(request::ChmodPath),
    ListAccounts(request::ListAccounts),
    Metadata(request::Metadata),
    DirectorySize(request::DirectorySize),
    GitContext(request::GitContext),
    GitStatus(request::GitStatus),
    GitDiff(request::GitDiff),
    MoveMetadata(request::MoveMetadata),
    DeleteMoveSource(request::DeleteMoveSource),
    OpenPath(request::OpenPath),
    Echo(request::Echo),
    AgentInfo(request::AgentInfo),
    GetAgentDetails(request::GetAgentDetails),
    GetLoggingLevel(request::GetLoggingLevel),
    SetLoggingLevel(request::SetLoggingLevel),
    Restart(request::Restart),
    SelfExec(request::SelfExec),
}

impl Command {
    /// Short operator-facing label without large or sensitive payloads.
    pub fn summary(&self) -> String {
        match self {
            Self::Ls(request) => match &request.path {
                Some(path) => format!("Ls path={path}"),
                None => "Ls path=.".to_string(),
            },
            Self::FileSearch(request) => format!(
                "FileSearch path={} query={} timeout={}s include_hidden={} respect_gitignore={} case_sensitivity={:?}",
                request.path,
                request.query,
                request.timeout_seconds,
                request.include_hidden,
                request.respect_gitignore,
                request.case_sensitivity
            ),
            Self::ContentGrep(request) => format!(
                "ContentGrep path={} query={} timeout={}s include_hidden={} respect_gitignore={} fixed_string={} case_sensitivity={:?} before_context={} after_context={}",
                request.path,
                request.query,
                request.timeout_seconds,
                request.include_hidden,
                request.respect_gitignore,
                request.fixed_string,
                request.case_sensitivity,
                request.before_context,
                request.after_context
            ),
            Self::RawDownload(request) => {
                let path = &request.path;
                match (request.range_start, request.range_end) {
                    (Some(start), Some(end)) => {
                        format!("RawDownload path={path} range={start}-{end}")
                    }
                    (Some(start), None) => format!("RawDownload path={path} range_start={start}"),
                    (None, Some(end)) => format!("RawDownload path={path} range_end={end}"),
                    (None, None) => format!("RawDownload path={path}"),
                }
            }
            Self::TarDownload(request) => format!(
                "TarDownload path={} include_root={}",
                request.path, request.include_root
            ),
            Self::Exec(_) => "Exec".to_owned(),
            Self::RawUpload(request) => format!(
                "RawUpload path={} on_existing={:?}",
                request.path, request.on_existing
            ),
            Self::EditFile(request) => format!("EditFile path={}", request.path),
            Self::TarUpload(request) => format!(
                "TarUpload path={} on_existing={:?}",
                request.path, request.on_existing
            ),
            Self::LocalCopyFile(request) => format!(
                "LocalCopyFile source={} dest={} on_existing={:?}",
                request.source_path, request.dest_path, request.on_existing
            ),
            Self::LocalCopyDirectory(request) => format!(
                "LocalCopyDirectory source={} dest={} on_existing={:?}",
                request.source_path, request.dest_path, request.on_existing
            ),
            Self::LocalMove(request) => format!(
                "LocalMove source={} dest={} directory={} on_existing={:?}",
                request.source_path,
                request.dest_path,
                request.source_is_directory,
                request.on_existing
            ),
            Self::RawDelete(request) => format!("RawDelete path={}", request.path),
            Self::Trash(request) => format!("Trash path={}", request.path),
            Self::ListTrash(_) => "ListTrash".to_string(),
            Self::EmptyTrash(_) => "EmptyTrash".to_string(),
            Self::RestoreTrash(request) => format!(
                "RestoreTrash location={} item={} destination={}",
                request.location_id, request.item_id, request.destination_path
            ),
            Self::CreateDirectory(request) => format!("CreateDirectory path={}", request.path),
            Self::RenamePath(request) => format!(
                "RenamePath dir={} old={} new={}",
                request.dir, request.old, request.new
            ),
            Self::ChownPath(request) => format!(
                "ChownPath path={} owner={:?} group={:?}",
                request.path, request.owner, request.group
            ),
            Self::ChmodPath(request) => format!(
                "ChmodPath path={} permissions={:#o}",
                request.path, request.permissions
            ),
            Self::ListAccounts(_) => "ListAccounts".to_string(),
            Self::Metadata(request) => format!("Metadata path={}", request.path),
            Self::DirectorySize(request) => format!(
                "DirectorySize path={} timeout={}s",
                request.path, request.timeout_seconds
            ),
            Self::GitContext(request) => format!("GitContext path={}", request.path),
            Self::GitStatus(request) => format!("GitStatus path={}", request.path),
            Self::GitDiff(request) => format!(
                "GitDiff files={} mode={:?}",
                request.files.len(),
                request.mode
            ),
            Self::MoveMetadata(request) => format!("MoveMetadata path={}", request.path),
            Self::DeleteMoveSource(request) => format!("DeleteMoveSource path={}", request.path),
            Self::OpenPath(request) => format!("OpenPath path={}", request.path),
            // Echo bodies can be large or sensitive, so only the command name is logged.
            Self::Echo(_) => "Echo".to_string(),
            Self::AgentInfo(_) => "AgentInfo".to_string(),
            Self::GetAgentDetails(_) => "GetAgentDetails".to_string(),
            Self::GetLoggingLevel(_) => "GetLoggingLevel".to_string(),
            Self::SetLoggingLevel(request) => format!("SetLoggingLevel level={}", request.level),
            Self::Restart(_) => "Restart".to_string(),
            Self::SelfExec(request) => format!("SelfExec path={}", request.path),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::search::ContentGrepResponse;

    /// A missing `include_root` keeps the flat archive copies rely on.
    #[test]
    fn tar_download_defaults_to_excluding_root() {
        let command: Command = serde_json::from_str(r#"{"type":"TarDownload","path":"test-dir"}"#)
            .expect("tar download command should deserialize");

        let Command::TarDownload(request) = command else {
            panic!("Expected TarDownload");
        };
        // The default must match what a copy's source download expects.
        assert!(!request.include_root);
    }

    /// Grep responses without context fields still decode, with empty context.
    #[test]
    fn content_grep_response_defaults_context_fields() {
        let response: ContentGrepResponse = serde_json::from_str(
            r#"{"results":[{"path":"/tmp/file","line_number":1,"line":"match","line_truncated":false}],"timed_out":false,"cancelled":false,"truncated":false,"omitted_long_lines":0,"duration_ms":1}"#,
        )
        .expect("grep response without context should deserialize");

        // A missing capability means the agent did not collect context lines.
        assert!(!response.context_supported);
        // Defaulted arrays keep zero-context results usable.
        assert!(response.results[0].before_context.is_empty());
        assert!(response.results[0].after_context.is_empty());
    }
}
