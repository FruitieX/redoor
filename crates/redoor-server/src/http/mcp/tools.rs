//! Schema-driven MCP device tool catalog and authorized dispatch.

use crate::actors::router::{CopyOperation, OutputStreamTracking, RouterMsg};
use base64::{Engine, engine::general_purpose::STANDARD};
use redoor_protocol::{
    api::{
        files::CreationOwnershipOptions,
        search::{CaseSensitivity, FindRequest, GrepRequest},
        transfers::{CopyEndpoint, CopyExistingMode},
    },
    ids::{AgentId, RequestId, TransferId},
    wire::{
        command::Command,
        exec::{ExecEvent, ExecRequest},
    },
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::{error::McpError, policy, schema::server_metadata};
use crate::http::{
    agent_client::{
        ContentGrep, CreateDirectory, DeletePath, FileSearch, ListPath, Listing, Metadata,
    },
    error::ApiError,
    machine_tokens::{MachinePrincipal, MachineScope},
    services::{
        output::OutputStream,
        transfer::{CopySpec, StartedCopy},
        upload::{AgentUpload, UploadStart},
    },
    state::ServerState,
};

/// Bounds the `hello` tool's echoed name so the greeting stays small.
const MAX_NAME_LENGTH: usize = 64;
/// Keeps inline MCP reads and writes small; larger files use the streaming endpoints.
const INLINE_FILE_LIMIT: usize = 64 * 1024;
/// Truncates directory listings so one large directory cannot blow up an MCP response.
const MAX_DIRECTORY_ENTRIES: usize = 2_000;

/// A compact definition keeps annotations and schemas consistent with implemented dispatch.
struct ToolDefinition {
    name: &'static str,
    description: &'static str,
    read_only: bool,
    destructive: bool,
    required: &'static [&'static str],
}

/// Single source for the advertised tool catalog and the names accepted by dispatch.
const TOOLS: &[ToolDefinition] = &[
    ToolDefinition {
        name: "hello",
        description: "Return a bounded greeting without accessing devices.",
        read_only: true,
        destructive: false,
        required: &[],
    },
    ToolDefinition {
        name: "list_devices",
        description: "List devices visible to this grant.",
        read_only: true,
        destructive: false,
        required: &[],
    },
    ToolDefinition {
        name: "get_device",
        description: "Get one visible device.",
        read_only: true,
        destructive: false,
        required: &["device_id"],
    },
    ToolDefinition {
        name: "run_command",
        description: "Run an argv-based non-PTY command.",
        read_only: false,
        destructive: true,
        required: &["device_id", "argv", "cwd"],
    },
    ToolDefinition {
        name: "get_operation",
        description: "Read principal-owned operation state.",
        read_only: true,
        destructive: false,
        required: &["operation_id"],
    },
    ToolDefinition {
        name: "read_command_output",
        description: "Read bounded retained command output.",
        read_only: true,
        destructive: false,
        required: &["operation_id"],
    },
    ToolDefinition {
        name: "cancel_operation",
        description: "Request cancellation of owned work.",
        read_only: false,
        destructive: true,
        required: &["operation_id"],
    },
    ToolDefinition {
        name: "list_directory",
        description: "List a bounded remote directory.",
        read_only: true,
        destructive: false,
        required: &["device_id", "path"],
    },
    ToolDefinition {
        name: "stat_path",
        description: "Read remote path metadata.",
        read_only: true,
        destructive: false,
        required: &["device_id", "path"],
    },
    ToolDefinition {
        name: "read_file",
        description: "Read a bounded remote file range.",
        read_only: true,
        destructive: false,
        required: &["device_id", "path"],
    },
    ToolDefinition {
        name: "write_file",
        description: "Write bounded content using explicit conflict policy.",
        read_only: false,
        destructive: true,
        required: &["device_id", "path", "content", "on_existing"],
    },
    ToolDefinition {
        name: "copy_path",
        description: "Copy a file or directory through the streaming pipeline.",
        read_only: false,
        destructive: false,
        required: &["source", "destination", "on_existing"],
    },
    ToolDefinition {
        name: "move_path",
        description: "Atomically rename or copy then delete a path.",
        read_only: false,
        destructive: true,
        required: &["source", "destination", "on_existing"],
    },
    ToolDefinition {
        name: "delete_path",
        description: "Trash or permanently delete an explicit path.",
        read_only: false,
        destructive: true,
        required: &["device_id", "path", "mode", "recursive"],
    },
    ToolDefinition {
        name: "create_directory",
        description: "Create a remote directory.",
        read_only: false,
        destructive: false,
        required: &["device_id", "path"],
    },
    ToolDefinition {
        name: "search_paths",
        description: "Search names below one absolute root.",
        read_only: true,
        destructive: false,
        required: &["device_id", "root", "query"],
    },
    ToolDefinition {
        name: "search_contents",
        description: "Search bounded text matches below one root.",
        read_only: true,
        destructive: false,
        required: &["device_id", "root", "query"],
    },
    ToolDefinition {
        name: "start_managed_agent",
        description: "Set a managed device's desired state to running.",
        read_only: false,
        destructive: false,
        required: &["device_id"],
    },
    ToolDefinition {
        name: "stop_managed_agent",
        description: "Stop a managed device and wait for cleanup.",
        read_only: false,
        destructive: true,
        required: &["device_id"],
    },
    ToolDefinition {
        name: "prepare_file_download",
        description: "Describe authenticated ranged download access.",
        read_only: true,
        destructive: false,
        required: &["device_id", "path"],
    },
    ToolDefinition {
        name: "prepare_file_upload",
        description: "Describe authenticated streamed upload access.",
        read_only: false,
        destructive: true,
        required: &["device_id", "path", "on_existing"],
    },
];

/// Returns deterministic schemas for every implemented device operation.
pub(super) fn list_result(params: &Value, id: &Value) -> Result<Value, McpError> {
    let object = object(params, "params", id)?;
    if object.get("cursor").is_some_and(|cursor| !cursor.is_null()) {
        return Err(invalid("The tool catalog has one page", id));
    }
    let tools = TOOLS.iter().map(|tool| json!({
        "name": tool.name, "description": tool.description,
        "inputSchema": {"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":input_properties(tool.name),"required":tool.required,"additionalProperties":true},
        "outputSchema": {"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"},
        "annotations": {"readOnlyHint":tool.read_only,"destructiveHint":tool.destructive,"idempotentHint":tool.read_only,"openWorldHint":tool.name == "run_command"}
    })).collect::<Vec<_>>();
    Ok(
        json!({"resultType":"complete","tools":tools,"ttlMs":300000,"cacheScope":"private","_meta":server_metadata()}),
    )
}

/// Dispatches one authorized tool without loopback HTTP or retained client session state.
pub(super) async fn call_result(
    state: &ServerState,
    principal: &MachinePrincipal,
    params: &Value,
    id: &Value,
) -> Result<Value, McpError> {
    let request = object(params, "params", id)?;
    let name = string(request, "name", id)?;
    if !TOOLS.iter().any(|tool| tool.name == name) {
        return Err(invalid(format!("Unknown tool: {name}"), id));
    }
    let empty = Value::Object(Map::new());
    let args = object(
        request.get("arguments").unwrap_or(&empty),
        "tool arguments",
        id,
    )?;
    let result = match name {
        "hello" => hello(args, id)?,
        "list_devices" => list_devices(state, principal, id).await?,
        "get_device" => get_device(state, principal, args, id).await?,
        "run_command" => run_command(state, principal, args, id).await?,
        "get_operation" => get_operation(state, principal, args, id).await?,
        "read_command_output" => read_command_output(state, principal, args, id).await?,
        "cancel_operation" => cancel_operation(state, principal, args, id).await?,
        "list_directory" => list_directory(state, principal, args, id).await?,
        "stat_path" => stat_path(state, principal, args, id).await?,
        "read_file" => read_file(state, principal, args, id).await?,
        "write_file" => write_file(state, principal, args, id).await?,
        "copy_path" => transfer(state, principal, args, id, false).await?,
        "move_path" => transfer(state, principal, args, id, true).await?,
        "delete_path" => delete_path(state, principal, args, id).await?,
        "create_directory" => create_directory(state, principal, args, id).await?,
        "search_paths" => search_paths(state, principal, args, id).await?,
        "search_contents" => search_contents(state, principal, args, id).await?,
        "start_managed_agent" => managed_agent(state, principal, args, id, true).await?,
        "stop_managed_agent" => managed_agent(state, principal, args, id, false).await?,
        "prepare_file_download" => prepare_transfer(principal, args, id, false)?,
        "prepare_file_upload" => prepare_transfer(principal, args, id, true)?,
        _ => unreachable!(),
    };
    Ok(complete(name, result))
}

/// Builds the required property subset while keeping optional arguments forward-compatible.
fn input_properties(name: &str) -> Value {
    let mut result = Map::new();
    for key in TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .into_iter()
        .flat_map(|tool| tool.required)
    {
        result.insert(
            (*key).to_string(),
            match *key {
                "argv" => json!({"type":"array","items":{"type":"string"},"minItems":1}),
                "source" | "destination" => {
                    json!({"type":"object","required":["device_id","path"]})
                }
                "recursive" => json!({"type":"boolean"}),
                _ => json!({"type":"string"}),
            },
        );
    }
    Value::Object(result)
}
/// Wraps structured tool output with a concise human-readable MCP summary.
fn complete(name: &str, value: Value) -> Value {
    let summary = if name == "hello" {
        value
            .get("greeting")
            .and_then(Value::as_str)
            .unwrap_or("Hello from Redoor!")
            .to_string()
    } else {
        format!("{name} completed")
    };
    json!({"resultType":"complete","content":[{"type":"text","text":summary}],"structuredContent":value,"isError":false,"_meta":server_metadata()})
}
/// Keeps the protocol smoke test harmless and independently bounded.
fn hello(args: &Map<String, Value>, id: &Value) -> Result<Value, McpError> {
    if args.keys().any(|key| key != "name") {
        return Err(invalid("hello accepts only the optional name argument", id));
    }
    let greeting = match args.get("name") {
        None => "Hello from Redoor!".to_string(),
        Some(Value::String(name))
            if !name.is_empty() && name.chars().count() <= MAX_NAME_LENGTH =>
        {
            format!("Hello, {name}, from Redoor!")
        }
        _ => return Err(invalid("hello name must contain 1 to 64 characters", id)),
    };
    Ok(json!({"greeting":greeting}))
}

/// Filters retained inventory before serialization so disallowed IDs are never disclosed.
async fn list_devices(
    state: &ServerState,
    principal: &MachinePrincipal,
    id: &Value,
) -> Result<Value, McpError> {
    require_scope(principal, MachineScope::DevicesRead, id)?;
    let devices = crate::http::agents::list_agent_snapshots(state)
        .await
        .map_err(|error| op(error.to_string(), id))?
        .into_iter()
        .filter(|agent| principal.grant.device_ids.contains(&agent.id.to_string()))
        .map(device_json)
        .collect::<Vec<_>>();
    Ok(json!({"devices":devices}))
}
/// Returns one explicitly granted device without exposing connection configuration.
async fn get_device(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<Value, McpError> {
    let device = string(args, "device_id", id)?;
    policy::require_device(principal, MachineScope::DevicesRead, device, id)?;
    let agent = crate::http::agents::list_agent_snapshots(state)
        .await
        .map_err(|error| op(error.to_string(), id))?
        .into_iter()
        .find(|agent| agent.id.to_string() == device)
        .ok_or_else(|| op("Device not found", id))?;
    Ok(json!({"device":device_json(agent)}))
}
/// Reduces the REST inventory model to the safe MCP device contract.
fn device_json(agent: redoor_protocol::api::agents::AgentInfoResponse) -> Value {
    json!({"device_id":agent.id,"display_name":agent.name,"status":agent.status,"managed":agent.managed,"default_working_directory":agent.cwd,"supported_features":{"trash":agent.supports_move_to_trash,"trash_restore":agent.supports_trash,"self_exec":agent.supports_self_exec}})
}

/// Admits foreground or independently owned background non-PTY execution.
async fn run_command(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<Value, McpError> {
    let device = string(args, "device_id", id)?.to_string();
    policy::require_device(principal, MachineScope::CommandsExecute, &device, id)?;
    let argv = args
        .get("argv")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("argv must be an array", id))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| invalid("argv values must be strings", id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cwd = absolute(string(args, "cwd", id)?, id)?.to_string();
    let env = args
        .get("environment")
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .map(|(key, value)| {
                    value
                        .as_str()
                        .map(|value| (key.clone(), value.to_string()))
                        .ok_or_else(|| invalid("environment values must be strings", id))
                })
                .collect()
        })
        .transpose()?
        .unwrap_or_default();
    let timeout_ms = args
        .get("deadline_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000);
    let output_limit = args
        .get("output_limit")
        .and_then(Value::as_u64)
        .unwrap_or(64 * 1024)
        .min(state.mcp.operations.per_job_quota() as u64) as usize;
    let background = args
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("foreground")
        == "background";
    let request = ExecRequest {
        argv,
        cwd: Some(cwd),
        env,
        timeout_ms: Some(timeout_ms),
    };
    request.validate().map_err(|error| invalid(error, id))?;
    let record = state
        .mcp
        .operations
        .create(principal.grant.id, vec![device.clone()], "command")
        .await
        .map_err(|error| {
            op(
                format!("Failed recording operation admission: {error:#}"),
                id,
            )
        })?;
    let (output, terminal) = match OutputStream::open_execution(
        &state.router_ref,
        AgentId::from(device.as_str()),
        request,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let mut current = record.lock().await;
            current.state = "failed".to_string();
            current.terminal = Some(json!({"reason":"admission_failed","error":error.to_string()}));
            drop(current);
            let _ = state.mcp.operations.update(&record).await;
            return Err(op("Execution admission failed", id));
        }
    };
    let remote_id = output.request_id();
    {
        let mut record = record.lock().await;
        record.remote_id = Some(remote_id.0);
        record.state = "running".to_string();
    }
    state
        .mcp
        .operations
        .update(&record)
        .await
        .map_err(|error| op(format!("Failed persisting execution: {error:#}"), id))?;
    let operation_id = record.lock().await.id;
    let task = drain_execution(
        state.mcp.operations.clone(),
        record.clone(),
        output,
        terminal,
        output_limit,
    );
    if background {
        tokio::spawn(task);
        Ok(json!({"operation_id":operation_id,"state":"running"}))
    } else {
        task.await;
        let record = record.lock().await;
        operation_json(&record)
    }
}

/// Drains output under backpressure even after the retention quota is exhausted.
async fn drain_execution(
    operations: super::operations::OperationRegistry,
    record: std::sync::Arc<tokio::sync::Mutex<super::operations::OperationRecord>>,
    mut output: OutputStream,
    mut terminal: tokio::sync::watch::Receiver<Option<ExecEvent>>,
    limit: usize,
) {
    let mut terminal_open = true;
    loop {
        tokio::select! {
            chunk = output.recv() => match chunk {
                Some(chunk) => {
                    let terminal_event = if chunk.is_last {
                        serde_json::from_slice::<ExecEvent>(&chunk.data).ok()
                    } else {
                        None
                    };
                    if operations.append_output(&record, &chunk.data, limit).await.is_err() {
                        record.lock().await.state = "failed".to_string();
                    }
                    if chunk.is_error { record.lock().await.state = "failed".to_string(); }
                    if let Some(event) = terminal_event {
                        finish_execution(&operations, &record, event).await;
                        output.disarm();
                        return;
                    }
                }
                None => {
                    if terminal_open && terminal.borrow().is_none() {
                        let _ = terminal.changed().await;
                    }
                    let event = { terminal.borrow_and_update().clone() };
                    if let Some(event) = event {
                        finish_execution(&operations, &record, event).await;
                        output.disarm();
                        return;
                    }
                    break;
                }
            },
            changed = terminal.changed(), if terminal_open => {
                if changed.is_err() {
                    terminal_open = false;
                    continue;
                }
                let event = { terminal.borrow_and_update().clone() };
                if let Some(event) = event {
                    finish_execution(&operations, &record, event).await;
                    output.disarm();
                    return;
                }
            }
        }
    }
    let mut current = record.lock().await;
    if current.terminal.is_none() {
        current.state = "unknown".to_string();
    }
    drop(current);
    let _ = operations.update(&record).await;
}

/// Records only an agent terminal acknowledgement as proof of process termination.
async fn finish_execution(
    operations: &super::operations::OperationRegistry,
    record: &std::sync::Arc<tokio::sync::Mutex<super::operations::OperationRecord>>,
    event: ExecEvent,
) {
    let mut current = record.lock().await;
    current.state = match event {
        ExecEvent::Exit { .. } => "completed",
        ExecEvent::Canceled => "canceled",
        ExecEvent::TimedOut => "timed_out",
        _ => "failed",
    }
    .to_string();
    current.remote_id = None;
    current.terminal = serde_json::to_value(event).ok();
    drop(current);
    let _ = operations.update(record).await;
}
/// Resolves an operation only after checking its stable grant owner.
async fn owned_operation(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<std::sync::Arc<tokio::sync::Mutex<super::operations::OperationRecord>>, McpError> {
    let operation_id = Uuid::parse_str(string(args, "operation_id", id)?)
        .map_err(|_| invalid("operation_id must be a UUID", id))?;
    let record = state
        .mcp
        .operations
        .get(operation_id)
        .await
        .map_err(|error| op(format!("Failed reading operation registry: {error:#}"), id))?
        .ok_or_else(|| McpError::operation_not_found("Operation not found", Some(id.clone())))?;
    policy::require_owner(principal, record.lock().await.owner_grant, id)?;
    Ok(record)
}
/// Returns durable state without depending on the router's prunable transfer history.
async fn get_operation(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<Value, McpError> {
    let record = owned_operation(state, principal, args, id).await?;
    {
        let current = record.lock().await;
        recheck_operation_access(principal, &current, id)?;
    }
    let record = record.lock().await;
    operation_json(&record)
}

/// Drains a retained terminal notification independently of client polling and router history.
async fn collect_transfer(
    operations: super::operations::OperationRegistry,
    record: std::sync::Arc<tokio::sync::Mutex<super::operations::OperationRecord>>,
    mut terminal: tokio::sync::watch::Receiver<
        Option<redoor_protocol::api::transfers::TransferProgressEntry>,
    >,
) {
    loop {
        let transfer = terminal.borrow_and_update().clone();
        if let Some(transfer) = transfer {
            let mut current = record.lock().await;
            current.state = match transfer.state {
                redoor_protocol::api::transfers::TransferProgressState::Canceled => "canceled",
                redoor_protocol::api::transfers::TransferProgressState::Errored => "failed",
                redoor_protocol::api::transfers::TransferProgressState::Completed => "completed",
                _ => "unknown",
            }
            .to_string();
            current.remote_id = None;
            current.terminal = Some(json!({
                "transferred_bytes": transfer.transferred_bytes,
                "total_bytes": transfer.total_bytes,
                "error": transfer.error,
                "atomic": transfer.atomic
            }));
            drop(current);
            if let Err(error) = operations.update(&record).await {
                redoor_core::log!(
                    redoor_protocol::wire::log::Level::Error,
                    "Failed persisting MCP transfer completion: {error:#}"
                );
            }
            return;
        }
        if terminal.changed().await.is_err() {
            let mut current = record.lock().await;
            current.state = "unknown".to_string();
            current.remote_id = None;
            drop(current);
            let _ = operations.update(&record).await;
            return;
        }
    }
}
/// Serializes lifecycle metadata without embedding retained output.
fn operation_json(record: &super::operations::OperationRecord) -> Result<Value, McpError> {
    Ok(
        json!({"operation_id":record.id,"kind":record.kind,"state":record.state,"termination":record.terminal,"output_size":record.output_size,"output_truncated":record.output_truncated,"cancel_requested":record.cancel_requested}),
    )
}
/// Reads one bounded binary-safe page from an owned command spool.
async fn read_command_output(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<Value, McpError> {
    let record = owned_operation(state, principal, args, id).await?;
    let record = {
        let current = record.lock().await;
        recheck_operation_access(principal, &current, id)?;
        current.clone()
    };
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(64 * 1024)
        .min(64 * 1024) as usize;
    let end = offset.saturating_add(limit).min(record.output_size);
    let data = state
        .mcp
        .operations
        .read_output(&record, offset, limit)
        .await
        .map_err(|error| op(format!("Failed reading retained output: {error}"), id))?;
    Ok(
        json!({"operation_id":record.id,"offset":offset,"next_offset":end,"more":end<record.output_size,"encoding":"base64","content":STANDARD.encode(data),"truncated":record.output_truncated}),
    )
}
/// Routes cancellation to the underlying execution or transfer without claiming termination.
async fn cancel_operation(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<Value, McpError> {
    let record = owned_operation(state, principal, args, id).await?;
    let mut current = record.lock().await;
    recheck_operation_access(principal, &current, id)?;
    if !matches!(
        current.state.as_str(),
        "admitted" | "running" | "canceling" | "unknown"
    ) {
        return Ok(json!({
            "operation_id": current.id,
            "cancellation_accepted": false,
            "termination_acknowledged": true
        }));
    }
    current.cancel_requested = true;
    let operation_id = current.id;
    let kind = current.kind.clone();
    let remote_id = current.remote_id;
    let device = current.device_ids.first().cloned();
    drop(current);
    state
        .mcp
        .operations
        .update(&record)
        .await
        .map_err(|error| op(format!("Failed persisting cancellation: {error:#}"), id))?;
    let accepted = match (kind.as_str(), remote_id, device.as_deref()) {
        ("command", Some(remote), Some(device)) => state
            .agents
            .cancel_execution(AgentId::from(device), RequestId::new(remote))
            .await
            .map_err(|error| op(error.to_string(), id))?,
        ("copy" | "move", Some(remote), _) => state
            .router_ref
            .request(|reply| RouterMsg::CancelPublicTransfer {
                transfer_id: TransferId::new(remote),
                reply,
            })
            .await
            .is_ok_and(|result| result.is_ok()),
        _ => false,
    };
    let current = record.lock().await;
    Ok(
        json!({"operation_id":operation_id,"cancellation_accepted":accepted,"termination_acknowledged":matches!(current.state.as_str(),"canceled"|"completed"|"failed"|"timed_out"|"interrupted")}),
    )
}

/// Bounds remote directory serialization independently of agent result size.
async fn list_directory(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<Value, McpError> {
    let (device, path) = device_path(args, id)?;
    policy::require_device(principal, MachineScope::FilesRead, device, id)?;
    let call = ListPath {
        path: path.to_string(),
    };
    match state
        .agents
        .call(&AgentId::from(device), call)
        .await
        .map_err(|error| op(error.to_string(), id))?
    {
        Listing::Directory(mut result) => {
            let truncated = result.files.len() > MAX_DIRECTORY_ENTRIES;
            result.files.truncate(MAX_DIRECTORY_ENTRIES);
            Ok(json!({"path":result.path,"entries":result.files,"truncated":truncated}))
        }
        Listing::File(_) => Err(op("Path is not a directory", id)),
    }
}
/// Maps agent metadata to a credential-free path contract.
async fn stat_path(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<Value, McpError> {
    let (device, path) = device_path(args, id)?;
    policy::require_device(principal, MachineScope::FilesRead, device, id)?;
    let result = state
        .agents
        .call(
            &AgentId::from(device),
            Metadata {
                path: path.to_string(),
            },
        )
        .await
        .map_err(|error| op(error.to_string(), id))?;
    Ok(
        json!({"path":result.path,"size":result.file_size,"is_file":result.is_file,"is_directory":result.is_dir,"mime_type":result.mime_type}),
    )
}
/// Reads one inline file page and preserves an exact byte continuation offset.
async fn read_file(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<Value, McpError> {
    let (device, path) = device_path(args, id)?;
    policy::require_device(principal, MachineScope::FilesRead, device, id)?;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(64 * 1024)
        .min(64 * 1024) as usize;
    let encoding = args
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("text");
    let mut stream = OutputStream::open(
        &state.router_ref,
        AgentId::from(device),
        Command::RawDownload {
            path: path.to_string(),
            range_start: Some(offset),
            // One byte past the page tells whether more content follows.
            range_end: Some(offset.saturating_add(limit as u64).saturating_add(1)),
        },
        OutputStreamTracking::Download {
            path: path.to_string(),
            total_bytes: limit as u64 + 1,
            full_size: None,
            resume_offset: Some(offset),
        },
    )
    .await
    .map_err(|error| op(error.to_string(), id))?;
    let page = stream
        .read_bounded(limit)
        .await
        .map_err(|error| op(error.0, id))?;
    // Dropping the stream cancels the agent if it still streams the look-ahead byte.
    drop(stream);
    let (bytes, more) = (page.data, page.overflowed);
    let next_offset = offset + bytes.len() as u64;
    let content = match encoding {
        "base64" => STANDARD.encode(&bytes),
        "text" => String::from_utf8(bytes)
            .map_err(|_| invalid("Requested range is not valid UTF-8; use base64", id))?,
        _ => return Err(invalid("encoding must be text or base64", id)),
    };
    Ok(
        json!({"path":path,"offset":offset,"next_offset":next_offset,"more":more,"encoding":encoding,"content":content}),
    )
}
/// Sends bounded inline content through the shared streamed upload service.
async fn write_file(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<Value, McpError> {
    let (device, path) = device_path(args, id)?;
    policy::require_device(principal, MachineScope::FilesWrite, device, id)?;
    let encoding = args
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("text");
    let content = string(args, "content", id)?;
    let bytes = if encoding == "base64" {
        STANDARD
            .decode(content)
            .map_err(|_| invalid("content is not valid base64", id))?
    } else {
        content.as_bytes().to_vec()
    };
    if bytes.len() > INLINE_FILE_LIMIT {
        return Err(invalid(
            "Inline writes are limited to 65536 bytes; use prepare_file_upload",
            id,
        ));
    }
    let command = Command::RawUpload {
        path: path.to_string(),
        on_existing: conflict_mode(string(args, "on_existing", id)?, id)?,
        ownership: CreationOwnershipOptions::default(),
    };
    let written = write_inline(state, device, path, command, &bytes)
        .await
        .map_err(|error| op(error.to_string(), id))?;
    Ok(json!({"path":path,"bytes_written":written}))
}

/// Writes bounded inline bytes through the shared streamed upload and returns the bytes written.
async fn write_inline(
    state: &ServerState,
    device: &str,
    path: &str,
    command: Command,
    bytes: &[u8],
) -> Result<u64, ApiError> {
    let upload = AgentUpload::start(
        &state.router_ref,
        AgentId::from(device),
        command,
        path.to_string(),
        bytes.len() as u64,
    )
    .await?;
    let UploadStart::Ready(mut upload) = upload else {
        return Ok(0);
    };
    upload.send(bytes).await?;
    upload.finish().await
}

/// Authorizes both endpoints before admitting copy or move work idempotently.
async fn transfer(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
    is_move: bool,
) -> Result<Value, McpError> {
    let source = object(
        args.get("source")
            .ok_or_else(|| invalid("source is required", id))?,
        "source",
        id,
    )?;
    let destination = object(
        args.get("destination")
            .ok_or_else(|| invalid("destination is required", id))?,
        "destination",
        id,
    )?;
    let source_device = string(source, "device_id", id)?;
    let source_path = absolute(string(source, "path", id)?, id)?;
    let destination_device = string(destination, "device_id", id)?;
    let destination_path = absolute(string(destination, "path", id)?, id)?;
    policy::require_device(principal, MachineScope::FilesRead, source_device, id)?;
    policy::require_device(principal, MachineScope::FilesWrite, destination_device, id)?;
    if is_move {
        policy::require_device(principal, MachineScope::FilesDelete, source_device, id)?
    }
    let mode = conflict_mode(string(args, "on_existing", id)?, id)?;
    let kind = if is_move { "move" } else { "copy" };
    let fingerprint = json!({"kind": kind, "arguments": args}).to_string();
    let key = args.get("idempotency_key").and_then(Value::as_str);
    let _admission = if let Some(key) = key {
        Some(
            state
                .mcp
                .operations
                .lock_admission(principal.grant.id, key)
                .await,
        )
    } else {
        None
    };
    if let Some(key) = key {
        match state
            .mcp
            .operations
            .idempotent_result(principal.grant.id, key, &fingerprint)
            .await
        {
            Ok(Some(result)) => return Ok(result),
            Err(()) => {
                return Err(invalid(
                    "idempotency_key was already used with different arguments",
                    id,
                ));
            }
            Ok(None) => {}
        }
    }
    let record = state
        .mcp
        .operations
        .create(
            principal.grant.id,
            vec![source_device.to_string(), destination_device.to_string()],
            kind,
        )
        .await
        .map_err(|error| {
            op(
                format!("Failed recording transfer admission: {error:#}"),
                id,
            )
        })?;
    let result = json!({"operation_id":record.lock().await.id,"state":"running"});
    // Persist the reservation before remote work so an ambiguous admission cannot be repeated.
    if let Some(key) = key {
        state
            .mcp
            .operations
            .remember(
                principal.grant.id,
                key.to_string(),
                fingerprint,
                result.clone(),
            )
            .await
            .map_err(|error| {
                op(
                    format!("Failed persisting idempotency record: {error:#}"),
                    id,
                )
            })?;
    }
    let spec = CopySpec {
        op: if is_move {
            CopyOperation::Move
        } else {
            CopyOperation::Copy
        },
        source: CopyEndpoint {
            agent: AgentId::from(source_device),
            path: source_path.to_string(),
        },
        dest: CopyEndpoint {
            agent: AgentId::from(destination_device),
            path: destination_path.to_string(),
        },
        on_existing: mode,
        observe_terminal: true,
    };
    let (remote_id, terminal) = match state.transfers.start_copy(spec).await {
        Ok(StartedCopy {
            transfer_id,
            terminal: Some(terminal),
        }) => (transfer_id.0, terminal),
        Ok(StartedCopy { terminal: None, .. }) => {
            return Err(op("Transfer admission lost its terminal observer", id));
        }
        Err(error) => {
            let mut current = record.lock().await;
            current.state = "failed".to_string();
            current.terminal = Some(json!({"reason":"admission_failed","error":error.to_string()}));
            drop(current);
            let _ = state.mcp.operations.update(&record).await;
            // Keyed callers share the durable admission even when its remote start fails.
            return if key.is_some() {
                Ok(result)
            } else {
                Err(op("Transfer admission failed", id))
            };
        }
    };
    {
        let mut record = record.lock().await;
        record.remote_id = Some(remote_id);
        record.state = "running".to_string();
    }
    tokio::spawn(collect_transfer(
        state.mcp.operations.clone(),
        record.clone(),
        terminal,
    ));
    state
        .mcp
        .operations
        .update(&record)
        .await
        .map_err(|error| op(format!("Failed persisting transfer: {error:#}"), id))?;
    Ok(result)
}
/// Requires explicit deletion mode and recursive intent before mutation.
async fn delete_path(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<Value, McpError> {
    let (device, path) = device_path(args, id)?;
    policy::require_device(principal, MachineScope::FilesDelete, device, id)?;
    let recursive = args
        .get("recursive")
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid("recursive must be an explicit boolean", id))?;
    let agent_id = AgentId::from(device);
    if !recursive {
        let metadata = state
            .agents
            .call(
                &agent_id,
                Metadata {
                    path: path.to_string(),
                },
            )
            .await
            .map_err(|error| op(error.to_string(), id))?;
        if metadata.is_dir {
            return Err(invalid(
                "recursive must be true when deleting a directory",
                id,
            ));
        }
    }
    let to_trash = match string(args, "mode", id)? {
        "permanent" => false,
        "trash" => true,
        _ => return Err(invalid("mode must be trash or permanent", id)),
    };
    let call = DeletePath {
        path: path.to_string(),
        to_trash,
    };
    state
        .agents
        .call(&agent_id, call)
        .await
        .map_err(|error| op(error.to_string(), id))?;
    Ok(json!({"path":path,"deleted":true}))
}
/// Creates one absolute directory through the same agent command used by REST.
async fn create_directory(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<Value, McpError> {
    let (device, path) = device_path(args, id)?;
    policy::require_device(principal, MachineScope::FilesWrite, device, id)?;
    let call = CreateDirectory {
        path: path.to_string(),
        ownership: CreationOwnershipOptions::default(),
    };
    state
        .agents
        .call(&AgentId::from(device), call)
        .await
        .map_err(|error| op(error.to_string(), id))?;
    Ok(json!({"path":path,"created":true}))
}
/// Runs the existing bounded platform-aware path search implementation.
async fn search_paths(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<Value, McpError> {
    let device = string(args, "device_id", id)?;
    let root = absolute(string(args, "root", id)?, id)?;
    policy::require_device(principal, MachineScope::FilesRead, device, id)?;
    let timeout = args
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .clamp(1, 60);
    let call = FileSearch {
        path: root.to_string(),
        request: FindRequest {
            query: string(args, "query", id)?.to_string(),
            timeout,
            include_hidden: args
                .get("include_hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            respect_gitignore: args
                .get("respect_gitignore")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            case_sensitivity: case_sensitivity(args, id)?,
        },
    };
    let mut result = state
        .agents
        .call(&AgentId::from(device), call)
        .await
        .map_err(|error| op(error.to_string(), id))?;
    let omitted = result.results.len().saturating_sub(2000);
    result.results.truncate(2000);
    Ok(
        json!({"matches":result.results,"timed_out":result.timed_out,"truncated":omitted>0,"omitted":omitted,"matching":"substring path/name matching using the connected agent's platform rules"}),
    )
}
/// Runs bounded grep while preserving regex, ignore, case, and context controls.
async fn search_contents(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
) -> Result<Value, McpError> {
    let device = string(args, "device_id", id)?;
    let root = absolute(string(args, "root", id)?, id)?;
    policy::require_device(principal, MachineScope::FilesRead, device, id)?;
    let timeout = args
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .clamp(1, 60);
    let request = GrepRequest {
        query: string(args, "query", id)?.to_string(),
        timeout,
        include_hidden: args
            .get("include_hidden")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        respect_gitignore: args
            .get("respect_gitignore")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        fixed_string: args
            .get("fixed_string")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        case_sensitivity: case_sensitivity(args, id)?,
        before_context: args
            .get("before_context")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(20),
        after_context: args
            .get("after_context")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(20),
    };
    let call = ContentGrep {
        path: root.to_string(),
        request,
    };
    let result = state
        .agents
        .call(&AgentId::from(device), call)
        .await
        .map_err(|error| op(error.to_string(), id))?;
    serde_json::to_value(result).map_err(|error| op(error.to_string(), id))
}
/// Applies existing watchdog desired-state semantics only to configured managed devices.
async fn managed_agent(
    state: &ServerState,
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
    start: bool,
) -> Result<Value, McpError> {
    let device = string(args, "device_id", id)?;
    policy::require_device(principal, MachineScope::AgentsManage, device, id)?;
    let agent_id = AgentId::from(device);
    let snapshot = crate::http::agents::managed_agent_snapshot(state, &agent_id)
        .await
        .map_err(|error| op(error.to_string(), id))?;
    let watchdog = state
        .watchdog_registry
        .lookup(&snapshot.name)
        .ok_or_else(|| op("Managed supervisor unavailable", id))?;
    if start {
        watchdog
            .start()
            .await
            .map_err(|error| op(error.to_string(), id))?;
        crate::http::agents::apply_managed_snapshot(state, agent_id, watchdog.snapshot())
            .await
            .map_err(|error| op(error.to_string(), id))?;
        Ok(json!({"device_id":device,"accepted":true,"poll_device":true}))
    } else {
        tokio::time::timeout(std::time::Duration::from_secs(10), watchdog.shutdown())
            .await
            .map_err(|_| op("Timed out stopping managed agent", id))?
            .map_err(|error| op(error.to_string(), id))?;
        crate::http::agents::apply_managed_snapshot(state, agent_id, watchdog.snapshot())
            .await
            .map_err(|error| op(error.to_string(), id))?;
        Ok(json!({"device_id":device,"cleanup_complete":true}))
    }
}
/// Describes the separately authenticated byte-stream request a client must perform.
fn prepare_transfer(
    principal: &MachinePrincipal,
    args: &Map<String, Value>,
    id: &Value,
    upload: bool,
) -> Result<Value, McpError> {
    let (device, path) = device_path(args, id)?;
    policy::require_device(
        principal,
        if upload {
            MachineScope::FilesWrite
        } else {
            MachineScope::FilesRead
        },
        device,
        id,
    )?;
    let conflict = args
        .get("on_existing")
        .and_then(Value::as_str)
        .unwrap_or("error");
    let suffix = if upload {
        let mode = conflict_mode(conflict, id)?;
        let normalized = match mode {
            CopyExistingMode::Error => "error",
            CopyExistingMode::Override => "override",
            CopyExistingMode::Merge => "merge",
        };
        format!("?on_existing={normalized}")
    } else {
        String::new()
    };
    Ok(
        json!({"method":if upload{"PUT"}else{"GET"},"endpoint":format!("/mcp/files/{device}{path}{suffix}"),"authentication":"Authorization: Bearer <the same MCP token>","range_supported":!upload,"content_length_required":upload,"expires_with_credential":true}),
    )
}

/// Rechecks current scopes and devices rather than trusting admission-time authority.
fn recheck_operation_access(
    principal: &MachinePrincipal,
    record: &super::operations::OperationRecord,
    id: &Value,
) -> Result<(), McpError> {
    for device in &record.device_ids {
        if !principal.grant.device_ids.contains(device) {
            return Err(McpError::forbidden(
                "Operation device is no longer included in this grant",
                Some(id.clone()),
            ));
        }
    }
    let required = match record.kind.as_str() {
        "command" => vec![MachineScope::CommandsExecute],
        "copy" => vec![MachineScope::FilesRead, MachineScope::FilesWrite],
        "move" => vec![
            MachineScope::FilesRead,
            MachineScope::FilesWrite,
            MachineScope::FilesDelete,
        ],
        _ => Vec::new(),
    };
    for scope in required {
        require_scope(principal, scope, id)?;
    }
    Ok(())
}
/// Parses the shared search case vocabulary without silent fallback for invalid values.
fn case_sensitivity(args: &Map<String, Value>, id: &Value) -> Result<CaseSensitivity, McpError> {
    match args
        .get("case_sensitivity")
        .and_then(Value::as_str)
        .unwrap_or("smart")
    {
        "sensitive" => Ok(CaseSensitivity::Sensitive),
        "insensitive" => Ok(CaseSensitivity::Insensitive),
        "smart" => Ok(CaseSensitivity::Smart),
        _ => Err(invalid(
            "case_sensitivity must be smart, sensitive, or insensitive",
            id,
        )),
    }
}
/// Converts explicit MCP conflict policy into the existing transfer policy.
fn conflict_mode(value: &str, id: &Value) -> Result<CopyExistingMode, McpError> {
    match value {
        "error" | "create" => Ok(CopyExistingMode::Error),
        "replace" | "override" => Ok(CopyExistingMode::Override),
        "merge" => Ok(CopyExistingMode::Merge),
        _ => Err(invalid("on_existing must be error, replace, or merge", id)),
    }
}
/// Parses the repeated explicit device/absolute-path pair used by file tools.
fn device_path<'a>(
    args: &'a Map<String, Value>,
    id: &Value,
) -> Result<(&'a str, &'a str), McpError> {
    Ok((
        string(args, "device_id", id)?,
        absolute(string(args, "path", id)?, id)?,
    ))
}
/// Rejects cwd-dependent and NUL-containing remote path syntax.
fn absolute<'a>(path: &'a str, id: &Value) -> Result<&'a str, McpError> {
    if !path.starts_with('/') || path.contains('\0') {
        Err(invalid("path must be absolute and contain no NUL", id))
    } else {
        Ok(path)
    }
}
/// Applies a scope-only check for inventory calls without a device target.
fn require_scope(
    principal: &MachinePrincipal,
    scope: MachineScope,
    id: &Value,
) -> Result<(), McpError> {
    if principal.grant.scopes.contains(&scope) {
        Ok(())
    } else {
        Err(McpError::forbidden(
            format!("Missing required scope {scope}"),
            Some(id.clone()),
        ))
    }
}
/// Provides uniform object validation for schema-driven dispatch.
fn object<'a>(
    value: &'a Value,
    name: &str,
    id: &Value,
) -> Result<&'a Map<String, Value>, McpError> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{name} must be an object"), id))
}
/// Extracts a nonempty string while retaining the JSON-RPC request identifier.
fn string<'a>(object: &'a Map<String, Value>, key: &str, id: &Value) -> Result<&'a str, McpError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid(format!("{key} must be a nonempty string"), id))
}
/// Constructs an argument error associated with this request.
fn invalid(message: impl Into<String>, id: &Value) -> McpError {
    McpError::invalid_params(message, Some(id.clone()))
}
/// Constructs a remote-operation error associated with this request.
fn op(message: impl Into<String>, id: &Value) -> McpError {
    McpError::operation(message, Some(id.clone()))
}
