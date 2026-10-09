//! Every one-shot agent command REST and MCP use, with its deadline and expected result.
//!
//! Deadlines live here and nowhere else: most commands use [`STANDARD_TIMEOUT`]; commands
//! with an agent-enforced time limit wait that limit plus [`AGENT_LIMIT_MARGIN`] so the agent
//! reports its own partial result first; process control uses [`QUICK_TIMEOUT`].

use super::AgentCall;
use crate::http::error::ApiError;
use redoor_protocol::{
    api::{
        agents::{AgentAccountsResponse, AgentDetailsResponse, EchoRequest, MountPoint},
        files::{
            ChmodPathResponse, ChownPathResponse, CreationOwnershipOptions, DirectorySizeResponse,
            MetadataResponse,
        },
        git::{GitContextResponse, GitDiffMode, GitDiffResponse, GitStatusResponse},
        search::{ContentGrepResponse, FileSearchResponse, FindRequest, GrepRequest},
        trash::TrashListResponse,
    },
    wire::{
        command::{
            Command, CommandResult, EchoResult, LsDirectoryResult, LsFileResult, MoveMetadataResult,
        },
        log::Level,
    },
};
use std::time::Duration;

/// Deadline for ordinary filesystem, git and inventory commands.
pub(crate) const STANDARD_TIMEOUT: Duration = Duration::from_secs(30);
/// Deadline for process control that answers immediately or not at all.
pub(crate) const QUICK_TIMEOUT: Duration = Duration::from_secs(5);
/// Deadline for permanently deleting every trash entry, which can touch many files.
pub(crate) const EMPTY_TRASH_TIMEOUT: Duration = Duration::from_secs(300);
/// Extra wait beyond an agent-enforced limit so the agent's own timeout result arrives first.
pub(crate) const AGENT_LIMIT_MARGIN: Duration = Duration::from_secs(2);

/// Waits for a command whose duration the agent bounds itself.
fn agent_limited(seconds: u64) -> Option<Duration> {
    Some(Duration::from_secs(seconds) + AGENT_LIMIT_MARGIN)
}

/// Lists a directory or describes a file (`Ls`).
pub(crate) struct ListPath {
    pub(crate) path: String,
}

/// What `Ls` found at a path.
pub(crate) enum Listing {
    Directory(LsDirectoryResult),
    File(LsFileResult),
}

impl AgentCall for ListPath {
    type Output = Listing;
    const OPERATION: &'static str = "listing a path";

    fn command(self) -> Command {
        Command::Ls {
            path: Some(self.path),
        }
    }

    fn extract(result: CommandResult) -> Result<Listing, ApiError> {
        match result {
            CommandResult::LsDirectory(listing) => Ok(Listing::Directory(listing)),
            CommandResult::LsFile(file) => Ok(Listing::File(file)),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Searches path names below a root.
pub(crate) struct FileSearch {
    pub(crate) path: String,
    pub(crate) request: FindRequest,
}

impl AgentCall for FileSearch {
    type Output = FileSearchResponse;
    const OPERATION: &'static str = "searching files";

    fn timeout(&self) -> Option<Duration> {
        agent_limited(self.request.timeout)
    }

    fn command(self) -> Command {
        Command::FileSearch {
            path: self.path,
            query: self.request.query,
            timeout_seconds: self.request.timeout,
            include_hidden: self.request.include_hidden,
            respect_gitignore: self.request.respect_gitignore,
            case_sensitivity: self.request.case_sensitivity,
        }
    }

    fn extract(result: CommandResult) -> Result<FileSearchResponse, ApiError> {
        match result {
            CommandResult::FileSearch(response) => Ok(response),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Searches file contents below a root.
pub(crate) struct ContentGrep {
    pub(crate) path: String,
    pub(crate) request: GrepRequest,
}

impl AgentCall for ContentGrep {
    type Output = ContentGrepResponse;
    const OPERATION: &'static str = "searching file contents";

    fn timeout(&self) -> Option<Duration> {
        agent_limited(self.request.timeout)
    }

    fn command(self) -> Command {
        Command::ContentGrep {
            path: self.path,
            query: self.request.query,
            timeout_seconds: self.request.timeout,
            include_hidden: self.request.include_hidden,
            respect_gitignore: self.request.respect_gitignore,
            fixed_string: self.request.fixed_string,
            case_sensitivity: self.request.case_sensitivity,
            before_context: self.request.before_context,
            after_context: self.request.after_context,
        }
    }

    fn extract(result: CommandResult) -> Result<ContentGrepResponse, ApiError> {
        match result {
            CommandResult::ContentGrep(response) => Ok(response),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Reads editor, download and copy metadata of one path.
pub(crate) struct Metadata {
    pub(crate) path: String,
}

impl AgentCall for Metadata {
    type Output = MetadataResponse;
    const OPERATION: &'static str = "reading metadata";

    fn command(self) -> Command {
        Command::Metadata { path: self.path }
    }

    fn extract(result: CommandResult) -> Result<MetadataResponse, ApiError> {
        match result {
            CommandResult::Metadata(metadata) => Ok(metadata),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Reads the size and filesystem identity a move needs to delete only what it copied.
pub(crate) struct MoveMetadata {
    pub(crate) path: String,
}

impl AgentCall for MoveMetadata {
    type Output = MoveMetadataResult;
    const OPERATION: &'static str = "reading move metadata";

    fn command(self) -> Command {
        Command::MoveMetadata { path: self.path }
    }

    fn extract(result: CommandResult) -> Result<MoveMetadataResult, ApiError> {
        match result {
            CommandResult::MoveMetadata(metadata) => Ok(metadata),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Round-trips a message, used to check that an agent answers.
pub(crate) struct Echo {
    pub(crate) request: EchoRequest,
}

impl AgentCall for Echo {
    type Output = EchoResult;
    const OPERATION: &'static str = "echoing";

    fn command(self) -> Command {
        Command::Echo {
            request: self.request,
        }
    }

    fn extract(result: CommandResult) -> Result<EchoResult, ApiError> {
        match result {
            CommandResult::Echo(echo) => Ok(echo),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Loads the host user and group catalog.
pub(crate) struct ListAccounts;

impl AgentCall for ListAccounts {
    type Output = AgentAccountsResponse;
    const OPERATION: &'static str = "listing accounts";

    fn command(self) -> Command {
        Command::ListAccounts
    }

    fn extract(result: CommandResult) -> Result<AgentAccountsResponse, ApiError> {
        match result {
            CommandResult::ListAccounts(accounts) => Ok(accounts),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Changes owner and/or group of a path.
pub(crate) struct ChownPath {
    pub(crate) path: String,
    pub(crate) owner: Option<String>,
    pub(crate) group: Option<String>,
}

impl AgentCall for ChownPath {
    type Output = ChownPathResponse;
    const OPERATION: &'static str = "changing ownership";

    fn command(self) -> Command {
        Command::ChownPath {
            path: self.path,
            owner: self.owner,
            group: self.group,
        }
    }

    fn extract(result: CommandResult) -> Result<ChownPathResponse, ApiError> {
        match result {
            CommandResult::ChownPath(response) => Ok(response),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Replaces the rwx bits of a path.
pub(crate) struct ChmodPath {
    pub(crate) path: String,
    pub(crate) permissions: u32,
}

impl AgentCall for ChmodPath {
    type Output = ChmodPathResponse;
    const OPERATION: &'static str = "changing permissions";

    fn command(self) -> Command {
        Command::ChmodPath {
            path: self.path,
            permissions: self.permissions,
        }
    }

    fn extract(result: CommandResult) -> Result<ChmodPathResponse, ApiError> {
        match result {
            CommandResult::ChmodPath(response) => Ok(response),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Opens a path with the agent desktop's native launcher.
pub(crate) struct OpenPath {
    pub(crate) path: String,
}

impl AgentCall for OpenPath {
    type Output = ();
    const OPERATION: &'static str = "opening a path";

    fn command(self) -> Command {
        Command::OpenPath { path: self.path }
    }

    fn extract(result: CommandResult) -> Result<(), ApiError> {
        match result {
            CommandResult::OpenPath => Ok(()),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Sums regular-file bytes below a directory within an agent-enforced limit.
pub(crate) struct DirectorySize {
    pub(crate) path: String,
    pub(crate) timeout_seconds: u64,
}

impl AgentCall for DirectorySize {
    type Output = DirectorySizeResponse;
    const OPERATION: &'static str = "calculating directory size";

    fn timeout(&self) -> Option<Duration> {
        agent_limited(self.timeout_seconds)
    }

    fn command(self) -> Command {
        Command::DirectorySize {
            path: self.path,
            timeout_seconds: self.timeout_seconds,
        }
    }

    fn extract(result: CommandResult) -> Result<DirectorySizeResponse, ApiError> {
        match result {
            CommandResult::DirectorySize(response) => Ok(response),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Deletes a path permanently or moves it to the platform trash.
pub(crate) struct DeletePath {
    pub(crate) path: String,
    pub(crate) to_trash: bool,
}

impl AgentCall for DeletePath {
    type Output = ();
    const OPERATION: &'static str = "deleting a path";

    fn command(self) -> Command {
        if self.to_trash {
            Command::Trash { path: self.path }
        } else {
            Command::RawDelete { path: self.path }
        }
    }

    fn extract(result: CommandResult) -> Result<(), ApiError> {
        match result {
            CommandResult::RawDelete | CommandResult::Trash => Ok(()),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Creates a directory with its parents.
pub(crate) struct CreateDirectory {
    pub(crate) path: String,
    pub(crate) ownership: CreationOwnershipOptions,
}

impl AgentCall for CreateDirectory {
    type Output = ();
    const OPERATION: &'static str = "creating a directory";

    fn command(self) -> Command {
        Command::CreateDirectory {
            path: self.path,
            ownership: self.ownership,
        }
    }

    fn extract(result: CommandResult) -> Result<(), ApiError> {
        match result {
            CommandResult::CreateDirectory => Ok(()),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Renames one entry within a directory.
pub(crate) struct RenamePath {
    pub(crate) dir: String,
    pub(crate) old: String,
    pub(crate) new: String,
}

impl AgentCall for RenamePath {
    type Output = ();
    const OPERATION: &'static str = "renaming a path";

    fn command(self) -> Command {
        Command::RenamePath {
            dir: self.dir,
            old: self.old,
            new: self.new,
        }
    }

    fn extract(result: CommandResult) -> Result<(), ApiError> {
        match result {
            CommandResult::RenamePath => Ok(()),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Lists trash inventory.
pub(crate) struct ListTrash;

impl AgentCall for ListTrash {
    type Output = TrashListResponse;
    const OPERATION: &'static str = "listing trash";

    fn command(self) -> Command {
        Command::ListTrash
    }

    fn extract(result: CommandResult) -> Result<TrashListResponse, ApiError> {
        match result {
            CommandResult::TrashList(response) => Ok(response),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Permanently deletes every trash entry and returns how many were removed.
pub(crate) struct EmptyTrash;

impl AgentCall for EmptyTrash {
    type Output = u64;
    const OPERATION: &'static str = "emptying trash";

    fn timeout(&self) -> Option<Duration> {
        Some(EMPTY_TRASH_TIMEOUT)
    }

    fn command(self) -> Command {
        Command::EmptyTrash
    }

    fn extract(result: CommandResult) -> Result<u64, ApiError> {
        match result {
            CommandResult::EmptyTrash { deleted_items } => Ok(deleted_items),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Restores one trash item and returns where it landed.
pub(crate) struct RestoreTrash {
    pub(crate) location_id: String,
    pub(crate) item_id: String,
    pub(crate) destination_path: String,
}

impl AgentCall for RestoreTrash {
    type Output = String;
    const OPERATION: &'static str = "restoring a trash item";

    /// A cross-device restore copies the whole item, so no fixed deadline fits; the agent can
    /// cancel it, which happens when the HTTP caller goes away.
    fn timeout(&self) -> Option<Duration> {
        None
    }

    fn command(self) -> Command {
        Command::RestoreTrash {
            location_id: self.location_id,
            item_id: self.item_id,
            destination_path: self.destination_path,
        }
    }

    fn extract(result: CommandResult) -> Result<String, ApiError> {
        match result {
            CommandResult::RestoreTrash { path } => Ok(path),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Classifies a path's repository membership.
pub(crate) struct GitContext {
    pub(crate) path: String,
}

impl AgentCall for GitContext {
    type Output = GitContextResponse;
    const OPERATION: &'static str = "reading git context";

    fn command(self) -> Command {
        Command::GitContext { path: self.path }
    }

    fn extract(result: CommandResult) -> Result<GitContextResponse, ApiError> {
        match result {
            CommandResult::GitContext(response) => Ok(response),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Lists repository changes below a directory.
pub(crate) struct GitStatus {
    pub(crate) path: String,
}

impl AgentCall for GitStatus {
    type Output = GitStatusResponse;
    const OPERATION: &'static str = "reading git status";

    fn command(self) -> Command {
        Command::GitStatus { path: self.path }
    }

    fn extract(result: CommandResult) -> Result<GitStatusResponse, ApiError> {
        match result {
            CommandResult::GitStatus(response) => Ok(response),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Diffs repository files.
pub(crate) struct GitDiff {
    pub(crate) files: Vec<String>,
    pub(crate) mode: GitDiffMode,
}

impl AgentCall for GitDiff {
    type Output = GitDiffResponse;
    const OPERATION: &'static str = "reading git diff";

    fn command(self) -> Command {
        Command::GitDiff {
            files: self.files,
            mode: self.mode,
        }
    }

    fn extract(result: CommandResult) -> Result<GitDiffResponse, ApiError> {
        match result {
            CommandResult::GitDiff(response) => Ok(response),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Reads the agent process's log threshold.
pub(crate) struct GetLoggingLevel;

impl AgentCall for GetLoggingLevel {
    type Output = Level;
    const OPERATION: &'static str = "reading the logging level";

    fn timeout(&self) -> Option<Duration> {
        Some(QUICK_TIMEOUT)
    }

    fn command(self) -> Command {
        Command::GetLoggingLevel
    }

    fn extract(result: CommandResult) -> Result<Level, ApiError> {
        match result {
            CommandResult::LoggingLevel(level) => Ok(level),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Changes the agent process's log threshold and returns the applied level.
pub(crate) struct SetLoggingLevel {
    pub(crate) level: Level,
}

impl AgentCall for SetLoggingLevel {
    type Output = Level;
    const OPERATION: &'static str = "changing the logging level";

    fn timeout(&self) -> Option<Duration> {
        Some(QUICK_TIMEOUT)
    }

    fn command(self) -> Command {
        Command::SetLoggingLevel { level: self.level }
    }

    fn extract(result: CommandResult) -> Result<Level, ApiError> {
        GetLoggingLevel::extract(result)
    }
}

/// Reads runtime details the agent reports about itself.
///
/// Identity fields owned by the registration (name, directory, platform, binary) are
/// overlaid by the caller from the router inventory, which is authoritative for them.
pub(crate) struct GetAgentDetails;

impl AgentCall for GetAgentDetails {
    type Output = AgentDetailsResponse;
    const OPERATION: &'static str = "fetching agent details";

    fn command(self) -> Command {
        Command::GetAgentDetails
    }

    fn extract(result: CommandResult) -> Result<AgentDetailsResponse, ApiError> {
        match result {
            CommandResult::GetAgentDetails(mut details) => {
                // Older connected agents may predate filtering, so enforce visibility here too.
                details.mount_points.retain(MountPoint::is_visible);
                Ok(*details)
            }
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Asks the agent to exec itself in place.
pub(crate) struct Restart;

impl AgentCall for Restart {
    type Output = ();
    const OPERATION: &'static str = "restarting the agent";

    fn timeout(&self) -> Option<Duration> {
        Some(QUICK_TIMEOUT)
    }

    fn command(self) -> Command {
        Command::Restart
    }

    fn extract(result: CommandResult) -> Result<(), ApiError> {
        match result {
            CommandResult::Restart => Ok(()),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}

/// Asks the agent to exec a freshly installed binary.
pub(crate) struct SelfExec {
    pub(crate) path: String,
}

impl AgentCall for SelfExec {
    type Output = ();
    const OPERATION: &'static str = "starting the upgraded agent";

    fn command(self) -> Command {
        Command::SelfExec { path: self.path }
    }

    fn extract(result: CommandResult) -> Result<(), ApiError> {
        match result {
            CommandResult::SelfExec { .. } => Ok(()),
            _ => Err(ApiError::Unexpected(Self::OPERATION)),
        }
    }
}
