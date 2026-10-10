//! Router mailbox messages and their request payloads; the router's whole public API.

use super::RouterError;
use super::state::CopyContentKind;
use super::streams::{ConnectionStreams, OutboundStream, OutputReceiver, StreamEnd};
use crate::actors::lanes::ControlLanes;
use crate::watchdog::WatchdogSnapshot;
use redoor_protocol::ids::{AgentId, RequestId, SocketId, TransferId, UnixTimestampSeconds};
use redoor_protocol::wire::log::LogStreamId;
use redoor_protocol::wire::terminal::{TerminalId, TerminalSize};
use redoor_protocol::{
    api::{
        agents::{AgentConnectionStatus, AgentDetailsResponse, AgentInfoResponse},
        transfers::TransferProgressListResponse,
        ui_events::UiEvent,
    },
    wire::{
        command::{Command, CommandResult},
        registration::{AgentCapabilities, AgentIdentity},
    },
};

/// One-shot reply port used by router request/reply messages.
pub type RouterReply<T> = tokio::sync::oneshot::Sender<T>;

/// Distinguishes public cancellation lookup failures from commit-boundary conflicts.
#[derive(Debug)]
pub enum CancelPublicTransferError {
    NotFound,
    NotCancelable,
    /// Failed cleanup admission must remain retryable rather than returning accepted cancellation.
    Delivery(RouterError),
}

/// Why the router refused an agent's `hello`; the router has already queued the matching
/// `rejected` frame on the session's lanes when the session receives this.
#[derive(Debug, Clone)]
pub struct RegistrationRejection {
    pub reason: redoor_protocol::wire::handshake::RejectReason,
    pub message: String,
}

/// Payload for registering one websocket-backed agent session with the router.
pub struct RegisterAgentRequest {
    /// Stable agent identifier used as the router registry key.
    pub agent_id: AgentId,
    /// Human-readable agent name shown in the UI.
    pub agent_name: String,
    /// Unique websocket session identifier for this connection.
    pub socket_id: SocketId,
    /// Typed command and priority lanes drained by the session writer.
    pub lanes: ControlLanes,
    /// Platform, account, start directory and binary announced at registration.
    pub identity: AgentIdentity,
    /// Optional protocol features announced at registration.
    pub capabilities: AgentCapabilities,
    /// Lets the router reject a managed registration whose shutdown won after socket parsing.
    pub watchdog: Option<crate::watchdog::WatchdogHandle>,
    /// Fences router commit against retry invalidating the parsed registration.
    pub watchdog_attempt_generation: Option<u64>,
    /// Payload stream tables of this control session.
    pub streams: ConnectionStreams,
    /// Tells the session whether the router accepted the agent, so a refused socket can be
    /// closed after its `rejected` frame was flushed; absent in unit tests.
    pub reply: Option<RouterReply<Result<(), RegistrationRejection>>>,
}

/// Retained inventory projection used by REST, tabs, and management controls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentListEntry {
    /// Stable agent identifier used in routes.
    pub id: AgentId,
    /// Human-readable agent name shown in tabs.
    pub name: String,
    /// Latest known or configured default directory.
    pub default_directory: Option<String>,
    /// Indicates whether the server owns a TOML supervisor for this id.
    pub managed: bool,
    /// Whether this retained entry can be changed through the SSH configuration API.
    pub configuration_editable: bool,
    /// Configured SSH destination when this inventory row is SSH-backed.
    pub ssh_target: Option<String>,
    /// Current public lifecycle state.
    pub status: AgentConnectionStatus,
    /// Start of the current authoritative connection.
    pub connected_at: Option<UnixTimestampSeconds>,
    /// Current socket generation so clients can distinguish fast reconnects.
    pub connection_id: Option<SocketId>,
    /// End of the most recent authoritative connection.
    pub last_seen_at: Option<UnixTimestampSeconds>,
    /// Latest managed lifecycle diagnostic.
    pub connection_issue: Option<String>,
    /// Sticky SSH start steps copied from the supervisor snapshot.
    pub provisioning_status: Vec<redoor_protocol::api::agents::ProvisioningStatusMessage>,
    /// Identity from the latest registration; absent until the agent first connects.
    pub identity: Option<AgentIdentity>,
    /// Capabilities from the latest registration; all off until the agent first connects.
    pub capabilities: AgentCapabilities,
}

impl AgentListEntry {
    /// Overwrites the registration-owned fields of agent-reported details.
    ///
    /// The control session's registration is the process-stable identity that list gating and
    /// routing already use, so details must agree with it rather than with a second report.
    pub(crate) fn apply_registration(&self, details: &mut AgentDetailsResponse) {
        details.id = self.id.clone();
        details.name = self.name.clone();
        if let Some(connected_at) = self.connected_at {
            details.connected_at = connected_at;
        }
        let Some(identity) = &self.identity else {
            return;
        };
        details.cwd = identity.default_directory.clone();
        details.os = identity.os.clone();
        details.arch = identity.arch.clone();
        details.hostname = identity.hostname.clone();
        details.username = identity.username.clone();
        details.binary = identity.binary.clone();
        if let Some(uid) = identity.uid {
            details.uid = uid;
        }
        details.is_root = identity.is_root;
    }
}

impl From<AgentListEntry> for AgentInfoResponse {
    /// Projects one inventory row onto the flat REST shape clients and bindings rely on.
    fn from(agent: AgentListEntry) -> Self {
        let identity = agent.identity;
        Self {
            id: agent.id,
            name: agent.name,
            cwd: agent.default_directory,
            managed: agent.managed,
            configuration_editable: agent.configuration_editable,
            ssh_target: agent.ssh_target,
            status: agent.status,
            connected_at: agent.connected_at,
            connection_id: agent.connection_id,
            last_seen_at: agent.last_seen_at,
            connection_issue: agent.connection_issue,
            provisioning_status: agent.provisioning_status,
            uid: identity.as_ref().and_then(|identity| identity.uid),
            is_root: identity.as_ref().is_some_and(|identity| identity.is_root),
            binary: identity.map(|identity| identity.binary),
            supports_self_exec: agent.capabilities.supports_self_exec,
            supports_native_open: agent.capabilities.supports_native_open,
            supports_move_to_trash: agent.capabilities.supports_move_to_trash,
            supports_trash: agent.capabilities.supports_trash,
        }
    }
}

/// Registers a configured entry before any process can be started.
pub struct RegisterManagedAgentRequest {
    /// Effective name is also the stable id for pre-connection routes.
    pub agent_id: AgentId,
    /// Configured directory may be absent for an SSH target.
    pub default_directory: Option<String>,
    /// Distinguishes SSH-backed entries supported by the managed-agent form from local entries.
    pub configuration_editable: bool,
    /// Configured SSH destination copied into inventory so list clients can label remotes.
    pub ssh_target: Option<String>,
    /// Acknowledges inventory visibility before HTTP serving begins.
    pub reply: RouterReply<()>,
}

/// Removes a stopped managed record after its TOML entry and supervisor are gone.
pub struct UnregisterManagedAgentRequest {
    /// Selects the retained inventory row that must disappear with the supervisor.
    pub agent_id: AgentId,
    /// Acknowledges cleanup so HTTP delete/replace can return only after inventory is gone.
    pub reply: RouterReply<()>,
}

/// Applies asynchronous supervisor state without waiting in the router task.
pub struct ApplyManagedLifecycleRequest {
    /// Selects the configured inventory record to update.
    pub agent_id: AgentId,
    /// Contains only small control-plane state, never process output.
    pub snapshot: WatchdogSnapshot,
    /// Removes the previous live connection at an explicit retry boundary.
    pub evict_existing: bool,
    /// Allows control endpoints to wait until inventory and socket cleanup are consistent.
    pub reply: Option<RouterReply<()>>,
}

/// Requests that a payload socket attach to the current control connection.
pub struct RegisterTransferConnectionRequest {
    /// Per-connection secret the socket presented during its upgrade; it identifies the
    /// control connection (and thereby the agent) the socket belongs to.
    pub token: String,
    /// Unique socket identity used to reject stale teardown.
    pub socket_id: SocketId,
    /// Bounded lane of stream frames, encoded only by the transfer socket writer.
    pub outgoing_binary: tokio::sync::mpsc::Sender<redoor_protocol::wire::stream::StreamChunk>,
    /// Stops an older or control-orphaned transfer task promptly.
    pub shutdown: tokio::sync::watch::Sender<bool>,
    /// Confirms that router state owns the connection before payload is accepted, and hands
    /// the socket its agent and the stream tables its reader delivers frames into.
    pub reply: RouterReply<Result<(AgentId, ConnectionStreams), RouterError>>,
}

/// Confirms destination upload setup before payload producers are allowed to run.
pub struct RouteTransferReadyRequest {
    /// Agent that registered the destination-side upload worker.
    pub agent_id: AgentId,
    /// Control session that reported readiness; the router ignores frames from any session
    /// other than the agent's current one, so a stale or forged socket cannot drive a flow.
    pub socket_id: SocketId,
    /// Upload request whose bounded worker channel is now available.
    pub request_id: RequestId,
}

/// Final command response routed back from an agent to the original caller.
pub struct RouteResponse {
    /// Agent that produced the response.
    pub agent_id: AgentId,
    /// Control session that delivered the response; only the agent's current session may
    /// resolve its flows.
    pub socket_id: SocketId,
    /// Internal request id used to correlate the response.
    pub request_id: RequestId,
    /// Final command result returned by the agent.
    pub result: CommandResult,
}

/// Registers one UI event stream that should receive router refresh events.
pub struct RegisterUiSubscriberRequest {
    /// Unique subscriber identifier used for later unregistering.
    pub subscriber_id: String,
    /// Event sink that receives UI refresh notifications.
    pub sender: tokio::sync::mpsc::UnboundedSender<UiEvent>,
}

/// Executes a one-shot command on an agent and replies with its final result.
pub struct ExecuteCommandRequest {
    /// Target agent that should execute the command.
    pub agent_id: AgentId,
    /// One-shot command to run on the agent.
    pub command: Command,
    /// Receives the request id once the command is queued, so a caller that gives up (deadline
    /// or dropped HTTP request) can cancel exactly this command. A dropped receiver means the
    /// caller already left and the router abandons the command right away.
    pub admitted: RouterReply<RequestId>,
    /// Reply port that receives the final command result.
    pub reply: RouterReply<CommandResult>,
}

/// Starts agent-to-REST output transport with explicit resource-specific tracking.
pub struct ExecuteStreamRequest {
    /// Target agent producing file bytes or execution events.
    pub agent_id: AgentId,
    /// Streaming command to run on the agent.
    pub command: Command,
    /// File progress is independent of transport; execution has no filesystem resource.
    pub tracking: OutputStreamTracking,
    /// Returns the stream's consumer, which reads frames without involving the router and
    /// carries the request id the caller needs to cancel on drop.
    pub reply: RouterReply<Result<OutputReceiver, RouterError>>,
}

/// Keeps filesystem progress metadata out of non-file output streams.
pub enum OutputStreamTracking {
    /// Downloads expose a filesystem resource and may resume a previous transfer.
    Download {
        path: String,
        total_bytes: u64,
        full_size: Option<u64>,
        resume_offset: Option<u64>,
    },
    /// Executions own a live output stream but do not create file-transfer history.
    Execution {
        /// Independent terminal acknowledgement stays observable when output is saturated.
        terminal: tokio::sync::watch::Sender<Option<redoor_protocol::wire::exec::ExecEvent>>,
    },
}

/// Outcome of waiting for an upload destination to become ready.
///
/// The request id is returned immediately from start so HTTP can arm cancellation
/// before this barrier. Init failures finish before `TransferReady`, so readiness
/// must still carry the same completion payload the HTTP handler maps to status
/// codes (permission denied -> 403, missing path -> 404, etc.).
#[derive(Debug)]
pub enum UploadStartOutcome {
    /// Destination worker exists; HTTP may begin forwarding the request body.
    Ready,
    /// Transfer ended before readiness; boxed so the ready path stays small.
    Finished(Box<Result<CommandResult, RouterError>>),
}

/// Starts a direct upload stream and returns the allocated internal request id.
pub struct StartUploadRequest {
    /// Target agent that should receive the upload.
    pub agent_id: AgentId,
    /// Streaming upload command to run on the agent.
    pub command: Command,
    /// Path used for progress reporting.
    pub path: String,
    /// Expected total byte count used for progress reporting.
    pub total_bytes: u64,
    /// Completion channel for the final upload result.
    pub completion_sender: tokio::sync::oneshot::Sender<Result<CommandResult, RouterError>>,
    /// Released once the destination worker is ready, or with early setup failure.
    ///
    /// Kept separate from `reply` so HTTP can arm cancel with the request id
    /// before blocking on this barrier.
    pub ready_sender: tokio::sync::oneshot::Sender<Result<UploadStartOutcome, RouterError>>,
    /// Immediate reply with the stream producer (or start rejection); the producer carries
    /// the request id so the caller can arm cancellation before the readiness barrier.
    pub reply: RouterReply<Result<OutboundStream, RouterError>>,
}

/// Transfers terminal edit delivery to the router while publishing the commit boundary.
pub struct CommitDirectUploadRequest {
    /// Agent receiving the edit bytes.
    pub agent_id: AgentId,
    /// Active direct edit request entering commit.
    pub request_id: RequestId,
    /// Terminal frame whose delivery authorizes the agent-side inode rewrite.
    pub chunk: redoor_protocol::wire::stream::StreamChunk,
    /// Completes after the terminal frame was accepted or its failure cleanup was started.
    pub reply: RouterReply<Result<(), RouterError>>,
}

/// Starts a local or remote copy operation managed by the router.
pub struct StartCopyRequest {
    /// Source agent that provides the file or directory contents.
    pub source_agent_id: AgentId,
    /// Source path to copy from.
    pub source_path: String,
    /// Destination agent that receives the copied contents.
    pub dest_agent_id: AgentId,
    /// Destination path to copy to.
    pub dest_path: String,
    /// Expected total byte count used for progress reporting.
    pub total_bytes: u64,
    /// Copy mode determining command/result mapping and payload kind.
    pub content_kind: CopyContentKind,
    /// Controls replacement when the destination path already exists.
    pub on_existing: redoor_protocol::api::transfers::CopyExistingMode,
    /// Selects whether copy completion is final or followed by source deletion.
    pub operation: super::state::CopyOperation,
    /// Prevents move cleanup from deleting a replacement created while copying.
    pub source_identity: Option<redoor_protocol::wire::command::MoveSourceIdentity>,
    /// Retains terminal progress independently of the prunable public history.
    pub terminal: Option<
        tokio::sync::watch::Sender<Option<redoor_protocol::api::transfers::TransferProgressEntry>>,
    >,
    /// Reply port that returns the public transfer id.
    pub reply: RouterReply<Result<TransferId, RouterError>>,
}

/// Progress notification emitted by an agent for copy-style transfers.
pub struct TransferProgressUpdateRequest {
    /// Agent that emitted the progress update.
    pub agent_id: AgentId,
    /// Control session that delivered the update; only the agent's current session counts.
    pub socket_id: SocketId,
    /// Internal request id the update belongs to.
    pub request_id: RequestId,
    /// Latest transferred byte count reported by the agent.
    pub transferred_bytes: u64,
    /// Updated total byte count if the agent learned it later.
    pub total_bytes: Option<u64>,
}

/// Why a server-to-agent stream stopped on the producer side.
#[derive(Debug)]
pub enum OutboundFailureReason {
    /// The agent's transfer lane closed while frames were still being sent.
    LaneClosed,
    /// The producer aborted the stream with an error frame (for example a body that exceeded
    /// its declared length).
    ProducerAborted(String),
}

/// Terminal event of a server-to-agent stream observed by its producer.
pub struct OutboundFailure {
    /// Agent receiving the stream.
    pub agent_id: AgentId,
    /// Stream that failed.
    pub request_id: RequestId,
    /// What went wrong.
    pub reason: OutboundFailureReason,
}

/// Requests a connected agent to establish one dedicated terminal socket.
pub struct OpenTerminalRequest {
    pub agent_id: AgentId,
    pub terminal_id: TerminalId,
    pub token: String,
    pub size: TerminalSize,
    pub cwd: String,
    pub reply: RouterReply<Result<(), RouterError>>,
}

/// Requests a connected agent to establish one dedicated log socket.
pub struct OpenAgentLogStreamRequest {
    pub agent_id: AgentId,
    pub log_stream_id: LogStreamId,
    pub token: String,
    pub reply: RouterReply<Result<(), RouterError>>,
}

/// Enumerates router work so live streams and lifecycle controls share explicit lanes.
pub enum RouterMsg {
    RegisterAgent(RegisterAgentRequest),
    /// Checks a transfer socket's upgrade token before the upgrade is accepted, so a socket
    /// with a stale or forged token is refused with `401` instead of being opened.
    AuthorizeTransferToken {
        token: String,
        reply: RouterReply<bool>,
    },
    RegisterTransferConnection(RegisterTransferConnectionRequest),
    UnregisterTransferConnection {
        agent_id: AgentId,
        /// Prevents a replaced transfer socket from clearing its successor.
        socket_id: SocketId,
    },
    RegisterManagedAgent(RegisterManagedAgentRequest),
    UnregisterManagedAgent(UnregisterManagedAgentRequest),
    ApplyManagedLifecycle(ApplyManagedLifecycleRequest),
    UnregisterAgent {
        agent_id: AgentId,
        /// Identifies which websocket session is unregistering so a stale
        /// session cannot accidentally remove a replacement connection that
        /// took over the same agent name.
        socket_id: SocketId,
    },
    RouteResponse(RouteResponse),
    RouteTransferReady(RouteTransferReadyRequest),
    GetAgentList {
        reply: RouterReply<Vec<AgentListEntry>>,
    },
    GetTransferProgress {
        reply: RouterReply<TransferProgressListResponse>,
    },
    RegisterUiSubscriber(RegisterUiSubscriberRequest),
    UnregisterUiSubscriber {
        subscriber_id: String,
    },
    ExecuteCommandRest(ExecuteCommandRequest),
    /// The transfer reader saw an inbound stream's terminal frame (or a credit violation).
    InboundStreamEnded(StreamEnd),
    /// A producer could not finish a server-to-agent stream.
    OutboundStreamFailed(OutboundFailure),
    ExecuteStreamCommandRest(ExecuteStreamRequest),
    /// Cancels only an execution owned by the specified agent, independently of file progress.
    CancelExecution {
        agent_id: AgentId,
        request_id: RequestId,
        reply: RouterReply<Result<bool, RouterError>>,
    },
    StartUploadStreamRest(StartUploadRequest),
    CommitDirectUpload(CommitDirectUploadRequest),
    /// Serializes staged publication with public cancellation before filesystem mutation.
    BeginUploadPublication {
        agent_id: AgentId,
        /// Session that asked; publication is authorized only for the agent's current session.
        socket_id: SocketId,
        request_id: RequestId,
    },
    CancelTransfer {
        agent_id: AgentId,
        request_id: RequestId,
    },
    CancelPublicTransfer {
        transfer_id: TransferId,
        reply: RouterReply<
            Result<
                redoor_protocol::api::transfers::CancelTransferResponse,
                CancelPublicTransferError,
            >,
        >,
    },
    StartCopyRest(StartCopyRequest),
    TransferProgressUpdate(TransferProgressUpdateRequest),
    OpenTerminal(OpenTerminalRequest),
    OpenAgentLogStream(OpenAgentLogStreamRequest),
    /// Periodic tick: prunes abandoned replies, publishes stream progress and releases
    /// throttled UI refreshes.
    CheckPendingUiRefresh,
    Shutdown,
}

#[cfg(test)]
impl RegisterAgentRequest {
    /// A plain registration for router and HTTP unit tests with every capability on.
    pub(crate) fn for_test(
        agent_id: &AgentId,
        lanes: ControlLanes,
        streams: ConnectionStreams,
    ) -> Self {
        Self {
            agent_id: agent_id.clone(),
            agent_name: agent_id.to_string(),
            socket_id: SocketId::new(),
            lanes,
            identity: AgentIdentity {
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                hostname: "host".to_string(),
                username: "user".to_string(),
                default_directory: "/tmp".to_string(),
                binary: redoor_core::identity::current_binary_identity(),
                uid: Some(1000),
                is_root: false,
            },
            capabilities: AgentCapabilities {
                supports_self_exec: true,
                supports_native_open: true,
                supports_trash: true,
                supports_move_to_trash: true,
            },
            watchdog: None,
            watchdog_attempt_generation: None,
            streams,
            reply: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redoor_protocol::api::agents::MountPoint;

    /// An inventory row of a connected agent.
    fn entry() -> AgentListEntry {
        let registration = RegisterAgentRequest::for_test(
            &AgentId::from("a"),
            ControlLanes::bounded(1, 1).0,
            ConnectionStreams::new(AgentId::from("a")),
        );
        AgentListEntry {
            id: AgentId::from("a"),
            name: "a".to_string(),
            default_directory: Some("/tmp".to_string()),
            managed: false,
            configuration_editable: false,
            ssh_target: None,
            status: AgentConnectionStatus::Connected,
            connected_at: Some(UnixTimestampSeconds::new(7)),
            connection_id: None,
            last_seen_at: None,
            connection_issue: None,
            provisioning_status: Vec::new(),
            identity: Some(registration.identity),
            capabilities: registration.capabilities,
        }
    }

    /// The REST projection stays flat and carries every identity and capability field.
    #[test]
    fn rest_projection_flattens_identity_and_capabilities() {
        let info = AgentInfoResponse::from(entry());
        // Identity fields reach the flat REST shape the UI and bindings expect.
        assert_eq!(info.uid, Some(1000));
        assert!(!info.is_root);
        assert!(info.binary.is_some());
        // Capability gates the UI uses survive the projection.
        assert!(info.supports_trash && info.supports_move_to_trash && info.supports_self_exec);
        // The JSON keeps the established flat field names, with no nested objects.
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["supports_native_open"], true);
        assert_eq!(json["uid"], 1000);
        assert!(json.get("identity").is_none() && json.get("capabilities").is_none());

        let mut never_connected = entry();
        never_connected.identity = None;
        never_connected.capabilities = AgentCapabilities::default();
        let info = AgentInfoResponse::from(never_connected);
        // Agents that never registered expose no identity and no capabilities.
        assert!(info.binary.is_none() && info.uid.is_none() && !info.supports_trash);
    }

    /// Agent-reported details are overridden by the registration the router trusts.
    #[test]
    fn registration_overrides_reported_details() {
        let mut details: AgentDetailsResponse = serde_json::from_value(serde_json::json!({
            "id": "other", "name": "", "pid": 1, "cwd": "/wrong", "config_path": "",
            "exe_path": "/bin/redoor", "load_average_one": 0.0, "load_average_five": 0.0,
            "load_average_fifteen": 0.0, "system_uptime": 1, "os": "?", "arch": "?",
            "hostname": "?", "external_ip": null, "username": "?", "connected_at": 0,
            "binary": serde_json::to_value(redoor_core::identity::current_binary_identity()).unwrap(),
            "mount_points": [], "uid": 0, "is_root": true,
        }))
        .unwrap();
        entry().apply_registration(&mut details);
        // Identity comes from the registration, not from the second self-report.
        assert_eq!(details.name, "a");
        assert_eq!(details.cwd, "/tmp");
        assert_eq!(details.os, "linux");
        assert_eq!(details.uid, 1000);
        assert!(!details.is_root);
        assert_eq!(details.connected_at, UnixTimestampSeconds::new(7));
        // Runtime-only fields stay as the agent reported them.
        assert_eq!(details.exe_path, "/bin/redoor");
        assert!(details.mount_points.iter().all(MountPoint::is_visible));
    }
}
