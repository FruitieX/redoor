# Workspace rewrite

Rewrite the Rust code of redoor into a cargo workspace and apply the architecture
review findings. The previous implementation lives in `../redoor_old` (identical to
`git show HEAD:src/...` in this repo) and is the behavioral reference. Port code from
it freely; the goal is a better structure, not new behavior.

The acceptance criterion for every phase: `mise exec -- pn test` passes (cargo fmt,
clippy `-D warnings`, cargo tests, TS types, lint, integration tests, Playwright).

## Hard constraints

- Follow `AGENTS.md` (comments on every fn/struct/enum explaining "why", tokio async
  APIs, `safe_rm_all` for recursive deletes, streaming only, no sleeps in tests,
  `tokio::select!` arms delegate to methods, etc.).
- The JSON wire format seen by `tests/`, `ui/` and `bindings/` must stay compatible.
  Tests act as fake agents over the raw websocket (`tests/agents.test.ts`,
  `tests/agent-upgrade.test.ts`), so message tags and field names must not change.
  New fields must be optional/additive. Splitting one Rust enum into two
  direction-specific enums must keep the exact same serde representation.
- Generated `bindings/*.ts` should stay byte-identical unless a type was deliberately
  changed; run `scripts/generate-ts-bindings` after touching `#[ts(export)]` types and
  check `git diff bindings`.
- `target/debug/redoor` must remain the produced binary (`cargo build` from the repo
  root). `scripts/release.mts` rewrites the single `^version = "..."$` line in the
  root `Cargo.toml`; keep exactly one such line there (`[workspace.package]`).
- Supported targets: Linux, macOS, Android (Termux). Keep `cfg(target_os = "android")`
  code compiling (cargo-ndk may not be installed locally; reason about it carefully).
- Only one `pn test` may run at a time on this machine (shared test ports, root/ssh
  fixtures, the `redoor-dev` user service). Agents running in parallel must limit
  themselves to `cargo` commands and must not run `pn test` / `pn integration-test` /
  `pn playwright` unless told they own the test slot.
- Flaky tests that pass on rerun go to `./flaky-tests.md`.
- `pn test` takes a long time: use a Bash timeout of at least 1200000 ms, or run it in
  the background and wait for completion. On failures, see `./log`.

## Target crate layout

```
Cargo.toml                 virtual manifest: [workspace] members = ["crates/*"],
                           [workspace.package] version, [workspace.dependencies]
.cargo/config.toml         [env] TS_RS_EXPORT_DIR = { value = "bindings", relative = true }
crates/
  redoor-protocol/         wire + REST DTOs, ts-rs exports. Deps: serde, ts-rs, uuid, jiff, bytes
    src/wire/              ServerToAgent, AgentToServer, Command, CommandResult,
                           CommandPolicy, exec/log/terminal/transfer framing (StreamChunk)
    src/api/               REST request/response types grouped by domain
                           (auth, mcp, agents, transfers, files, git, ui_events, ...)
  redoor-core/             logging, safe_fs, app_name, paths (home dir), process helpers,
                           subprocess::run_checked, websocket timeouts, server_address,
                           bounded tail reader, shared tar helpers, config (+ edit/import),
                           RuntimeContext, test_support
  redoor-agent/            agent runtime, command implementations (fs, git, search, grep,
                           metadata, identity), transfers, trash, platform module
  redoor-server/           axum server, router actor, sessions, watchdog, ssh provisioning,
                           binaries, MCP, auth, ui embedding
  redoor-cli/              `redoor remote ...` client (reqwest). Must not depend on
                           redoor-agent or redoor-server.
  redoor/                  the binary: clap dispatch, service management
                           (ServiceBackend trait + systemd/launchd backends), process
                           control, build.rs, tests/remote_cli.rs
```

Dependency direction: protocol <- core <- {agent, server, cli} <- redoor.
Agent and server must not depend on each other. Resolve old cycles (config <-> ssh,
config <-> process_control, remote -> agent) by moving the shared pieces down into
core/protocol.

## Findings to implement (by phase)

### Phase 1 – Workspace split (mechanical)

Move code into the crate layout above with minimal behavior change. Split the
2,400-line `commands.rs` into `redoor-protocol::wire` (agent protocol) and
`redoor-protocol::api` (REST DTOs, placed by domain), with the command implementations
(`CommandHandler`, git, file_search, content_grep, metadata, identity) in redoor-agent.
`build.rs`, rust-embed paths and `tests/remote_cli.rs` move into `crates/redoor`.
Narrow `tokio` features per crate where easy. Bindings must be byte-identical.

### Phase 2 – Agent runtime

1. One shared `Command::policy() -> CommandPolicy { lane, cancel_scope, exclusive }`
   in redoor-protocol: a single exhaustive match replacing `is_upload`,
   `requires_transfer`, `is_local_transfer`, `handles_cancellation` and the server's
   `DirectUploadKind::from_command`. Both server and agent use it.
2. A single dispatcher on the agent: reads `policy()` once, admits, registers
   cancellation, spawns, logs and responds in one place. Commands implement a
   `run(self, ctx: CommandCtx)` style API (grouped by domain modules); remove the
   "handled by the agent runtime" stub branches and dead fallbacks.
3. A typed `Responder` (wraps the outbound channel: `respond(CommandResult)`,
   `progress(..)`, `transfer_ready()`); a recording test responder for unit tests.
4. `ServerToAgent` / `AgentToServer` enums (same JSON). Parse failures are logged;
   if a `request_id` can be recovered, reply `CommandResult::Error { kind: InvalidInput }`.
5. Delete the field-less `AgentActor`; split `AgentState` into `ControlLink`,
   `TransferLink` and `Sessions` with methods (`invalidate()` etc.) instead of 20
   `pub(crate)` fields and duplicated clear blocks.
6. One `CancelRegistry<K>` built on `tokio_util::sync::CancellationToken` (child tokens
   of a per-connection parent) replacing the five `Arc<Mutex<HashMap<_, watch::Sender>>>`
   registries; RAII `Registration` that unregisters on drop.
7. Restart/self-exec is an explicit post-send action on the outbound frame
   (`OutboundFrame { msg, after_send: Option<PostSend> }`) set by the dispatcher, not
   detected by parsing outgoing JSON in the websocket writer.
8. Generic transfer drivers: `UploadSink` (raw file, tar dir, edit) +
   `drive_upload`, `ByteSource` (file range, tar pipe) + `drive_download`, owning the
   cancel -> abort -> unregister -> respond ordering in one place. `StagingPath` type that
   deletes itself on drop replaces the six temp/staging path helpers.
9. `TrashProvider` trait with `FreedesktopTrash` (Linux **and Android/Termux**),
   `MacTrash`, `UnsupportedTrash`, chosen once. `platform` module (mounts, accounts,
   identity) instead of `cfg` blocks in business logic. Single
   `MoveSourceIdentity::from_metadata`.

### Phase 3 – Server router and transfers

1. `RouterHandle::send` must never silently drop on a full mailbox: async contexts use
   `send_async`; `send` returns `RouterSendError::{Full, Stopped}`.
2. Per-stream flow control so one slow consumer cannot stall other streams of the
   same agent: the transfer socket reader never awaits a consumer. Agents advertise an
   optional `supports_stream_credit` capability at registration; for such agents the
   server grants credit with a new control message (`stream_credit { request_id,
   frames }`) as consumers drain, and the agent's per-stream writer waits for credit
   (window ~4 x 64 KiB). Agents without the capability (e.g. the fake agents in tests)
   keep working with the old behavior.
3. Data bypasses the router actor: the router handles admission, lifecycle and
   cancellation and hands out a `StreamLease { id, link, cancel: CancellationToken,
   bytes: Arc<AtomicU64> }`. Chunks are forwarded directly; progress is read from the
   atomic; only terminal events go back to the router. Prune pending REST requests on
   a timer, not on every message.
4. One flow table: `HashMap<RequestId, FlowId>` + `HashMap<FlowId, Flow>` with
   `enum Flow { Command, Output, Upload, Copy }` and explicit phase enums
   (`UploadPhase::{AwaitingReady, Streaming, Committing, Publishing, Canceling, Done}`).
   Move = `CopyFlow { then: DeleteSource }`. Disconnect cleanup = one `retain`.
   Router state uses methods, not free functions over `&mut RouterState`.
   Router message handlers that do not await are sync (`clippy::unused_async`).
5. Typed `AgentClient` with `trait AgentCall { type Output; fn command(self);
   fn extract(CommandResult) -> Result<Output, ApiError> }` replacing hard-coded
   timeouts, hand-built `ExecuteCommandRest`, and copy-pasted "Unexpected result type"
   arms. `ApiError: IntoResponse`; no `{:?}` debug strings shown to users. On timeout,
   send a cancel to the agent.
6. `server/services/`: `TransferService::start_copy(CopySpec { op: Copy | Move })`
   shared by REST copy, REST move and MCP; `OutputStream::open(..)` owning the receiver
   plus an armed cancel guard and implementing `Stream<Item = Bytes>`; REST and MCP are
   thin adapters.
7. `AgentIdentity` + `AgentCapabilities` structs embedded (`#[serde(flatten)]` on the
   wire) instead of copying ~12 fields across four structs.
8. Typed outbound to the agent (`AgentLink` / `Outbound` enum) serialized in the
   session writer, so router tests do not parse JSON.

### Phase 4 – Infrastructure

1. Every role function returns `anyhow::Result<ExitCode>`; `main` prints `{:#}` once.
   No `process::exit`/`panic!` in startup paths; `PidFile` cleans up in `Drop`.
   `thiserror` in library modules, `anyhow` only at the binary/CLI edge, no
   `Box<dyn Error>` / `Result<_, String>` APIs.
2. `run_server` split into `ServerConfig::resolve(args, file)` (pure, tested;
   CLI > env > file > default), `ServerRuntime::build(cfg)` and `serve_until_restart()`.
   `AuthMode` enum instead of optional username/password pairs.
3. Config read via serde (`deny_unknown_fields`, `alias`, `toml_edit::de`) + a
   `validate()` pass; `toml_edit::DocumentMut` only for edits. Paths as `&Path`.
4. `ServiceBackend` trait with systemd and launchd backends, shared `ServiceState`
   formatter, shared `paths::home()` and `subprocess::run_checked`.
5. `RuntimeContext { app_name, paths, ws_timeouts, ... }` built once in `main` and
   passed down instead of process-wide `OnceLock` globals where feasible (the `log!`
   macro may stay global but writes through an injectable logger).
6. Watchdog: `SpawnError` enum instead of `String`, bounded supervisor channel, one
   `read_bounded_tail` in core.
7. SSH: extract the relay loop into `relay.rs` with a `Relay` struct; break up
   `prepare_ssh_backed_agent_for_destination`.
8. Dependencies: replace `fs2` with std `File::lock`; drop redundant hash/rand crates
   where possible; check `cargo tree -d`.

## Keep (do not regress)

Separate control and binary transfer sockets with a `biased` priority lane; generation
/ socket-id stale-event guards; early-cancel memory; bounded channels and bounded
diagnostics everywhere; watchdog actor-per-agent design; single-binary askpass
re-entry; `toml_edit` for format-preserving edits; `RouterError` -> HTTP mapping in one
place; `bounded_tar`, `destination.rs` symlink-safe merges, `atomic_rename.rs`.

## Process

Each phase ends with `pn test` green and one or more commits on the
`workspace-rewrite` branch whose messages explain why. Record deviations from this plan
in the "Log" section below.

## Log

### Phase 1 – Workspace split (done)

- `redoor-protocol`: `ids` (AgentId, RequestId, ...), `identity` (BinaryIdentity,
  ServerBuildMode), `wire` (`Message` envelope; `command` = Command/CommandResult/
  CommandErrorKind + agent-only result structs; `exec`, `log` (Level, LogEntry,
  LogErrorDetails, LogEvent, limits), `stream` (StreamChunk framing), `terminal`,
  `transfer`), `api::{agents, auth, diffs, exec, files, git, logging, mcp, search, server,
  trash, transfers, ui_events, user_state}` + `api::ErrorResponse`.
- `redoor-core`: logging, safe_fs, app_name, server_address, websocket timeouts, process
  (re-exec + ProcessSlot + MANAGED_AGENT_ENV/DETACHED_ENV), atomic_rename, destination
  (was `agent::transfers::destination`), config (+ bootstrap/edit/import/local_agent,
  now also owns SshBackedAgentConfig/default_agent_name), desktop, systemd_notify,
  identity (current_binary_identity/current_exe_path/external_ip), `test_support` behind
  the `test-support` feature.
- `redoor-agent`: old `agent/` at crate root, `commands/` (CommandHandler, git,
  file_search, content_grep, metadata, agent config path), ownership,
  directory_measurement.
- `redoor-server`: actors (router/session), watchdog, registries, ssh, binaries,
  `http/` (old `server/`), `startup::run_server` (moved out of main.rs).
- `redoor-cli`: old `remote/` at crate root. `redoor`: main, process_control,
  process_logs, service_management, systemd, launchd, tests/remote_cli.rs.
- Cycles broken: config<->ssh (ssh config struct moved into core config),
  config<->process_control (ProcessSlot/env markers moved into core process),
  remote->agent (destination helpers moved into core).
- Deviations: there is no build.rs in `crates/redoor`. `env!`/`RustEmbed` resolve in the
  crate that uses them, so `redoor-core/build.rs` emits the git/build identity and
  `redoor-server/build.rs` emits the `ui/dist` rerun directives (rust-embed folder is
  `../../ui/dist/`). Orphan-rule `From<config> for Managed*ConfigurationResponse` impls
  moved into core config. `ssh::provision` now hard-codes the `redoor` package name and
  resolves the debug binary from the workspace root. Pre-existing `clippy --all-targets`
  test lints (items after test module, held MutexGuard, large Err in tungstenite
  callbacks, no-op drop) were fixed so `cargo clippy --workspace --all-targets -- -D
  warnings` is clean. tokio narrowing: only protocol drops tokio entirely.
- Later phases: protocol still depends on anyhow (StreamChunk parsing) and thiserror;
  `rand_core` stays in core only to enable `getrandom` for argon2's `OsRng`; Android
  could not be type-checked locally (no NDK C toolchain for aws-lc-sys).

### Phase 2 – Agent runtime (done)

- `redoor-protocol::wire::policy`: `Command::policy() -> CommandPolicy { lane, cancel_scope,
  exclusive }` with `CommandLane::{Control, Output, Upload(UploadKind)}`,
  `CancelScope::{Connection, Request, Uninterruptible}` and `exclusive` (= serialized trash
  mutations: Trash, EmptyTrash, RestoreTrash). One exhaustive match; unit tests check sample
  coverage of every variant and consistency rules. `UploadKind` owns `direction()`,
  `completion_matches()` and `payload_kind()`; the server's `DirectUploadKind` is gone and the
  router/HTTP upload code reads `command.policy().upload_kind()`.
- `wire::Message` split into `ServerToAgent` / `AgentToServer` (identical JSON, golden-string
  tests). `ServerToAgent::unparsed_command_request_id` lets the agent answer an unparsable
  `command` frame with `CommandResult::Error { kind: InvalidInput }`; other parse failures are
  logged. Server matches are now exhaustive (an unreachable `_ => {}` disappeared).
  `MoveSourceIdentity::from_metadata` replaces the two agent copies.
- `redoor-core::staging`: `StagingKind` (one naming scheme `.{name}.{tag}-{random}`, temp-dir
  names for edits) and `StagingPath` (owned, `discard().await` on failure paths, `keep()` after
  publication, `Drop` spawns removal as the abort fallback). Override backups only borrow the
  name generator (never drop-deleted, they may hold user data).
- `redoor-agent` layout:
  - `runtime/{mod, links, sessions, cancel, dispatch, responder}.rs`: `AgentRuntime` (event loop,
    connect, frame handling) with `ControlLink` / `TransferLink` (generations, `attach`/`begin`/
    `lose`/`invalidate`) and `Sessions` (request/terminal/log registries, early-cancel tombstones,
    `invalidate_transfers()` / `invalidate()`); one `invalidate_connection()` replaces the two
    clear-everything blocks. `AgentActor`, `AgentState`, `protocol.rs`, `messages.rs`, `raw/`
    and `TerminalFinished`/`LogStreamFinished` messages are gone (RAII registrations).
  - `CancelRegistry<K>` on `CancellationToken`: generation token per registry (child of the
    control connection scope), per-key child tokens, RAII `Registration` (serial-checked drop),
    `revoked()` token distinguishing teardown from a per-request cancel, optional capacity.
    One request registry replaces the upload/download/local-transfer maps, so `cancel_transfer`
    is a single lookup; file search / grep just get connection child tokens.
  - `Dispatcher`: reads the policy once; admission (transfer link, 32-command cap that uploads
    bypass), registration, early-cancel, exclusive lock, spawn into `commands`/`uploads`
    JoinSets (both joined on teardown), abort of `Connection`-scope commands via
    `run_until_cancelled`, and delivery of `Reply::{Result, Streamed, Fallback}`.
  - `Responder` + typed `OutboundFrame { frame: ControlFrame, after_send: Option<PostSend> }`;
    the control writer serializes and runs `PostSend::Reexec` only after a successful write
    (tests use a recording closure). `Responder::recording()` / `CommandCtx::detached()` give
    typed frames to unit tests.
  - `commands/mod.rs`: `RunCommand for Command` is the only command match; implementations moved
    to domain modules (`fs`, `host`, `git`, `file_search`, `content_grep`, `metadata`, trash,
    transfers). `CommandHandler`, stub "handled by the runtime" arms and the dead
    FileSearch/ContentGrep fallbacks are gone.
  - `transfers/`: `UploadSink` trait + `UploadDriver` (raw `RawFileSink`, tar `TarDirectorySink`,
    edit `EditSession`; `PublicationGate` and `UploadRoutes` for chunk/publication routing),
    `ByteSource` trait + `DownloadDriver` (`FileRangeSource`, `TarDirectorySource`), and
    `StreamWriter` (per-request framed writer used by downloads and exec). Cancel/failure order is
    now uniform: stop -> abort/finish producer -> unregister -> respond.
  - `trash/`: `TrashProvider` trait (boxed futures, "unsupported" defaults) with
    `FreedesktopTrash` (Linux and Android), `MacTrash`, `UnsupportedTrash`; `Trash::select`
    chooses once at startup. `platform/{mounts, accounts, identity}` holds the Android/Termux
    constraints (no mount capacity, no per-mount trash discovery, empty account catalog).
- Deviations / behavior notes:
  - Downloads are now `Request`-scope cooperative instead of being dropped on disconnect; their
    terminal error frame is raced against `revoked()` so teardown never waits on a full queue.
  - Upload setup (destination checks, staging creation) runs in the spawned upload task instead
    of inline in the actor; readiness is still sent only after the chunk route exists.
  - Duplicate request ids answer `AlreadyExists "Request is already running for request_id=N"`.
  - Every `Reply::Result` logs "Command complete"; the "Received transfer cancel from server"
    log line is kept for any registry hit (tests wait for it).
  - Android and macOS were type-checked locally with `cargo check --target
    aarch64-linux-android` / `aarch64-apple-darwin` using a no-op C compiler and archiver for
    aws-lc-sys (`CC_<target>=fakecc AR_<target>=fakear AWS_LC_SYS_NO_ASM=1
    AWS_LC_SYS_CMAKE_BUILDER=0`; the fake tools write empty objects/archives, which works because
    `check` never links). `redoor-core`'s lib tests still reference a non-Android function on
    Android (pre-existing; non-test targets are clean).
- Notes for Phase 3:
  - Credit gate: `transfers::stream::StreamWriter::send_frames` is the only egress for
    agent-to-server payload frames (downloads and exec). Put the "await credit for one frame"
    immediately before each `transfer.send(...)` there; the writer already knows its request id.
    A credit grant can be routed like publication decisions (a map in `AgentServices` keyed by
    request id, fed from a new `ServerToAgent::StreamCredit` arm in
    `AgentRuntime::handle_server_frame`).
  - Server should drive admission/flow kind from `Command::policy()`: `requires_transfer()` for
    the transfer-link check, `upload_kind()` for upload flows (done), `lane == Output` for
    output-stream flows (the `streaming::start` command/tracking validation still uses
    `matches!`), and `cancel_scope == Request` to decide whether `cancel_transfer` is meaningful.

### Phase 3a – Router data path (done)

Items 1–4 of Phase 3, plus the pre-existing MCP rate-limit failure. Items 5–8 are part B.

- Item 1 (`RouterHandle::send`): returns `RouterSendError::{Full, Stopped}` instead of folding a
  full mailbox into the shutdown error. Every async caller now uses `send_async` (agent
  registration and `TransferProgressUpdate` in the session, MCP read cancellation, the former
  chunk completions). `request()` no longer sends a best-effort prune on timeout.
- Item 5 (MCP 429): the limiter is correctly keyed per credential. The retention test shared
  `deviceToken` with the preceding workflow test, which (with a 20 ms expiry poll worth ~100
  requests) spent the 120/min budget, so its first call failed in every file run. A dedicated
  credential then exposed a second latent cause: ten parallel admissions on one credential exceed
  its concurrency cap of eight. The test now owns two credentials (the retention ceiling it checks
  is global) and polls expiry every 100 ms. No limits were changed.
- Item 3 (data path, `actors/router/streams.rs`): the router admits, tracks lifecycle and cancels;
  payload never enters its mailbox (the 1024 mailbox now carries control messages only).
  - `StreamLease { id, link: AgentLink, cancel: CancellationToken, bytes: Arc<AtomicU64>, fault }`
    is shared by the router and the data path. `AgentLink` holds the priority lane (credit
    grants), the transfer lane and the negotiated credit flag.
  - `ConnectionStreams` (per control session: inbound slots + outbound credit gates behind short
    `std::sync::Mutex`es) is created by the session at registration, stored in
    `AgentConnection.streams`, returned to the transfer socket by `RegisterTransferConnection`, and
    used by the session to apply agent credit grants without the router.
  - Inbound (download, exec, copy source): `deliver()` in the transfer reader looks up the slot and
    `try_send`s; only terminal frames (`StreamEnd { outcome: Completed | Failed | Overflow,
    delivered, received_bytes }`) go to the router as `InboundStreamEnded`. Consumers use
    `OutputReceiver::recv()` (cancel safe; counts drained bytes, returns credit).
  - Outbound (upload, edit, copy destination): `OutboundStream::send_frames()` writes directly to
    the agent's transfer lane after spending credit; producer failures go to the router as
    `OutboundStreamFailed { LaneClosed | ProducerAborted(msg) }`. The edit commit terminal is still
    a router decision, sent by a router-spawned task (caller drop cannot strand it).
  - Remote copies run a `CopyForwarder` task (source `OutputReceiver` -> destination
    `OutboundStream`); it owns destination chunk numbering and sends the abort frame when the
    router fails the copy (`lease.fail(reason)`).
  - Progress is pulled from the lease atomics on `GetTransferProgress`, before terminal
    transitions, and on the 250 ms tick (which also drives throttled UI refreshes and prunes
    abandoned command replies — no per-message `retain`).
- Item 2 (credit protocol):
  - Negotiation: agents send `supports_stream_credit: true` in `agent_register` (optional, default
    false); the server confirms with `stream_credit: true` in `transfer_socket_open` (omitted when
    false). Both directions switch together per control session. Missing capability (fake test
    agents, old agents) or missing confirmation (old servers) keeps the old behavior: the reader
    waits for a depth-one consumer, the agent never waits for credit.
  - Messages: `ServerToAgent::StreamCredit { request_id, frames }` (`{"type":"stream_credit",...}`,
    priority lane) for agent-to-server streams; `AgentToServer::StreamCredit { request_id, frames }`
    (agent control lane) for server-to-agent streams. Grants for finished streams are ignored.
  - Window: `STREAM_CREDIT_WINDOW_FRAMES = 4` non-terminal frames of up to 64 KiB in flight per
    stream; receivers return credit in batches of `STREAM_CREDIT_BATCH_FRAMES = 2` as the consumer
    drains (server: when the HTTP/copy consumer receives; agent: after the upload sink staged the
    chunk). Receive queues hold `window + 1` frames: the terminal frame (last or error) never spends
    credit, so cancellation acknowledgements cannot wait for a grant. Shared `CreditGate`
    (semaphore, grants clamped to the window) and `CreditLedger` live in `redoor-core::stream_credit`.
  - Overflow is a per-stream protocol violation: the server fails that stream (consumer gets an
    error frame, exec terminal gets `Error`, agent gets `cancel_transfer`); the agent unroutes that
    upload so its worker fails. The socket and other streams continue.
  - Agent side: `StreamWriter::with_credit` (gate registered in `AgentServices::output_credits`,
    RAII removal), `UploadRoutes::open(.., credited)`, `Responder::stream_credit`.
- Item 4 (flow table, `actors/router/flows.rs`): `FlowTable { by_request: HashMap<RequestId, FlowId>,
  flows: HashMap<FlowId, Flow> }`, `enum Flow { Command, Output, Upload, Copy(Box<_>) }`,
  `OutputPhase::{Streaming, Canceling}`, `UploadPhase::{AwaitingReady, Streaming, Committing,
  Publishing, Canceling { explicit }}`, `CopyPhase::{Local, Remote(Box<RemoteCopy>),
  DeletingSource}` with `RemoteStage::{AwaitingDestination { source_command }, Streaming,
  Canceling { source_acked, dest_acked }}`; a move is `CopyFlow { then: Some(DeleteSource) }`.
  `route_response` is one index lookup. Agent loss (control or transfer) is one `extract_if` pass
  (`fail_agent_flows(agent, AgentLoss::{Control, Transfer(reason)})`) plus per-flow settlement.
  All router code is methods on `RouterState` (agents, cleanup, output, progress, ui,
  transfers/{upload, copy, move}); every handler is synchronous and `clippy::unused_async` is
  enabled for the whole server crate (one watchdog function fixed).
- Tests: Rust unit tests for the credit gate/ledger, stream tables (stalled consumer, overflow,
  stopped streams, uncredited fallback, outbound credit), output/upload/copy flows, the copy
  forwarder with a stalled destination, agent writer and upload overflow; the extended
  `slow_upload_send_does_not_block_unrelated_router_work` also stalls a download consumer and
  checks an exec stream still flows. New `tests/stream-isolation.test.ts` stalls a 64 MiB download
  body and requires another download and an exec to complete (it times out on the pre-change code).
- Deviations / behavior notes:
  - `FlowId` is the first request id of a flow; there is no `UploadPhase::Done` (a flow is removed
    when its terminal result is routed).
  - `ExecuteStreamRequest` replies with an `OutputReceiver` and `StartUploadRequest` with an
    `OutboundStream` (both carry the request id); `chunk_sender`/`rest_cancel_sender` are gone. An
    explicit download cancel stops the lease, so the body ends after at most the already queued
    frames (instead of immediately).
  - Download progress counts bytes the consumer drained (resume offsets stay accurate); completion
    records every byte the reader queued, so unknown-size archives complete with their full size.
  - Copy teardown on failure cancels a source that has not finished (previously a lost or failed
    destination left the source streaming into nothing). A copy source that answers on the control
    lane with an error now fails the copy (previously it hung). A remote copy canceled after its
    source already finished counts that completion as the source's acknowledgement (previously it
    could stay `Canceling`). A move without a captured source identity is rejected instead of
    panicking the router.
  - A closed transfer lane seen by a producer is settled by the transfer-socket cleanup (copy
    forwarder) or `OutboundStreamFailed` (HTTP uploads).
- Notes for part B:
  - `OutputReceiver` (lease + credit) is the natural core of `OutputStream::open`; `AgentUpload` +
    `OutboundStream` of `TransferService`. `OutputCancelGuard`/`UploadCancelGuard` still live in HTTP.
  - JSON is still built in the router (`AgentConnection::send_to_lane`, `queue_priority_cancel`) and
    in the data path (`AgentLink::grant`); typed `Outbound` should cover `StreamCredit` too.
  - `supports_stream_credit` belongs in the `AgentCapabilities` struct; every
    `RegisterAgentRequest` literal (tests included) now needs `streams: ConnectionStreams`.
  - Abandoned command replies are pruned by the tick; `AgentClient` timeouts should still send a
    cancel to the agent. `{:?}` error strings remain in exec/raw/MCP handlers.

### Phase 3b – Server API layer (done)

Items 5–8 of Phase 3.

- Item 5 (`http/agent_client.rs`, `http/agent_client/calls.rs`, `http/error.rs`):
  - `trait AgentCall { type Output; const OPERATION; fn timeout(&self) -> Option<Duration>;
    fn command(self) -> Command; fn extract(CommandResult) -> Result<Output, ApiError> }` with one
    call type per agent command REST/MCP use (~26). `AgentClient::call` maps
    `CommandResult::Error` to `ApiError::Command` before `extract`, so `extract` only names the
    one success variant. Deadlines live only in `calls.rs`: `STANDARD_TIMEOUT` 30 s, `QUICK_TIMEOUT`
    5 s (restart, logging level), `EMPTY_TRASH_TIMEOUT` 300 s, agent-limited calls (find, grep,
    directory size) wait their own limit + `AGENT_LIMIT_MARGIN` 2 s, `RestoreTrash` has none.
  - Router replies (admission, inventory, cancel, progress) share `ROUTER_REPLY_TIMEOUT` (10 s):
    `RouterHandle::request(build)` lost its timeout parameter because every handler answers
    synchronously. `RouterCallError` implements `Display` (`TimedOut` no longer wraps `Elapsed`).
  - `ApiError::{Command, Router(RouterError), RouterUnavailable(RouterCallError), Timeout(Duration),
    Unexpected(&'static str), Status { status, message }}` + `IntoResponse` (same `ErrorResponse`
    body); `status_code()` is the single REST error-status mapping (reuses
    `RouterError::status_code`). `responses.rs` is gone. Almost every handler now returns
    `Result<Json<T>, ApiError>`; `require_absolute_path`, `required_content_length` and upload
    request tokens return `ApiError` too.
  - Timeout/abandon: `ExecuteCommandRequest` gained `admitted: oneshot<RequestId>`. A
    `PendingCommand` drop guard in `AgentClient` sends `RouterMsg::CancelTransfer` when the caller
    stops waiting (deadline or dropped HTTP future); the router's `abandon_command` removes the
    command flow and queues `cancel_transfer` only for `CancelScope::Request` commands (the only
    ones the agent registers; others are short or agent-bounded). A caller gone before admission
    is abandoned by the router itself when the `admitted` send fails.
  - The router's `GetAgentDetails` rewrite is gone: `AgentClient::agent_details` overlays the
    registration-owned fields from the inventory (`AgentListEntry::apply_registration`); mount
    filtering is in the `GetAgentDetails` call's `extract`.
  - No `{:?}` in user-visible errors remain in the HTTP layer (handlers, exec, raw, MCP, managed
    inventory errors).
- Item 6 (`http/services/`):
  - `transfer.rs`: `TransferService::start_copy(CopySpec { op: CopyOperation, source, dest,
    on_existing, observe_terminal })` validates paths and same-endpoint copies, reads `Metadata`
    or `MoveMetadata` (identity) and admits the flow; used by REST copy, REST move and MCP
    `copy_path`/`move_path`. `ServerState` holds `agents: AgentClient` and `transfers`.
  - `output.rs`: `OutputStream::open(router, agent, command, tracking)` /
    `open_execution(..)` own the `OutputReceiver` and an armed cancel guard that disarms on a
    terminal frame; `recv` (cancel safe, via a `ReusableBoxFuture` kept in the stream), `peek`,
    `read_bounded(limit)` and `impl Stream<Item = io::Result<Bytes>>`. Used by file/archive
    downloads, diffs, REST exec and MCP exec/read_file (MCP reads now cancel the agent on drop).
  - `upload.rs`: `AgentUpload` (moved from `raw/upload.rs`) with `start -> UploadStart::{Ready,
    Completed}`, `send`, `forward_body`, `finish -> bytes` (checks the completion kind), all
    returning `ApiError`; used by raw PUT, archive, edit, MCP write_file and agent upgrades.
  - `mcp/services.rs` and `http/streaming.rs` were deleted; `raw::upload` is a public module
    instead of a re-export shim.
- Item 7: `redoor_protocol::wire::registration::{AgentIdentity, AgentCapabilities}` (no `TS`),
  flattened into `AgentToServer::AgentRegister` (golden round-trip test; `cwd` is a serde rename of
  `default_directory`; every capability defaults to false). `supports_stream_credit` is a
  capability; `AgentCapabilities::normalized()` keeps "inventory implies move" in one place.
  `RegisterAgentRequest`, `AgentConnection`, `KnownAgent` (`identity: Option<_>`) and
  `AgentListEntry` embed the two values. Decision: the REST `AgentInfoResponse` stays a flat DTO
  built by one `From<AgentListEntry>` (flattening would have turned the binding into an
  intersection type), so `bindings/` is byte-identical.
- Item 8 (`actors/lanes.rs`): `ControlLanes { commands, priority }` carry typed `ServerToAgent`
  frames (`try_command`, `try_priority`, `try_priority_or_command`, `queue_priority`,
  `send_priority`); the transfer lane carries `StreamChunk`. The session writer (`encode_frame`)
  and the transfer writer are the only places that serialize. `AgentLink` holds `ControlLanes`, so
  credit grants are typed too. No extra `Outbound` enum was needed: `ServerToAgent` is the typed
  outbound frame. Router, stream, copy, output and session tests assert typed frames.
- Tests: unit tests for `ApiError` mapping, `AgentClient` (agent failure and wrong-variant
  extraction, timeout -> cancel for request-scoped commands, no cancel otherwise),
  `OutputStream` (drop cancels, terminal disarms, byte stream error, bounded read + cancel),
  `TransferService` (validation, special files, move identity forwarding), registration JSON
  round trip, REST projection and details overlay, lanes (retention, fallback) and the writer
  encoding. `http/fake_agent.rs` is a shared scripted agent on a real router for HTTP unit tests;
  `RegisterAgentRequest::for_test` replaces the copied registration literals. Integration (new):
  REST copy and move reject special files with an operation-specific 400. Full `pn test` green
  on the first run, no flakes.
- Behavior notes: agent deadlines answer 504 "Agent did not respond within N seconds"
  (previously 500 with a Debug string; logging level was 503); router unavailability answers 503;
  protocol mismatches answer 500 "Unexpected agent response while ...". MCP copy/move now reject
  identical endpoints like REST, and MCP admission failures record the real error text.
- Notes for Phase 4:
  - Remaining `Result<_, String>` APIs in HTTP: watchdog lifecycle helpers
    (`apply_managed_snapshot`, `watchdog.start()`), `managed_agent_snapshot`'s tuple error and the
    agent configuration handlers; these are the natural next `ApiError`/`thiserror` conversions.
  - `ROUTER_REPLY_TIMEOUT` and the call deadlines are constants; `RuntimeContext` could carry them
    if tests ever need shorter values.

### Phase 4 – Infrastructure (done)

- Item 1 (errors/exits): `main` is a plain `fn main() -> ExitCode` that builds the Tokio runtime,
  runs the selected role (`anyhow::Result<ExitCode>`), prints a failure once with `{:#}` and
  shuts the runtime down in the background (a blocking stdin reader must not keep a finished
  process alive). `run_utility`/`run_role` exit paths, the server's dozen `eprintln!` +
  `process::exit` blocks, the bind `panic!` and the serve `unwrap` are gone; `PidFile` removes its
  claim in `Drop`. The agent returns `AgentExit::{Shutdown, Rejected}` instead of exiting on a
  server error; askpass returns an `ExitCode`. Typed errors where callers match or map:
  `ConfigError` (core), `WatchdogError` and `SpawnError` (watchdog), `ProvisionError` (ssh),
  `OpenError` (desktop; the agent matches `NoDesktop` instead of a substring),
  `SubprocessError` (core), `InvalidLogSetting` (logging), `AgentStreamError` (output stream);
  `From<WatchdogError> for ApiError`. The managed-agent start/shutdown/retry routes, the
  `managed_agent_snapshot`/`apply_managed_snapshot` helpers and all agent configuration
  handlers now return `ApiError` (no more `(StatusCode, String)` tuples). No `Box<dyn Error>`
  remains.
- Item 2 (startup split, `redoor-server/src/startup.rs`): `ConfigLocation::find_or_bootstrap`,
  pure `ServerConfig::resolve(args, ServerEnvironment, file, path, paths)` (unit tests for CLI >
  env > file > default, invalid env, missing `[server]`), `ServerRuntime::build` (logging,
  auth/MCP storage, actors, listener, inventory, each failure with context) and
  `serve_until_restart` (stops managed children before the re-exec). `config::AuthMode
  { Configured { username, password }, #[cfg(linux)] SystemAccount }` replaces the optional
  username/password pair; `LoginCredentials` and both `unreachable!`s are gone. Agent side:
  `locate_config` (shared by daemon preparation and startup) + `AgentSettings::resolve`
  (unit-tested precedence).
- Item 3 (config): `config/file.rs` holds serde `Raw*` structs with `deny_unknown_fields`, read
  with `toml_edit::de` (the `serde` feature), plus one `validate` pass producing the public types.
  `parse_config_file(&Path)`; `toml_edit::DocumentMut` only in `edit.rs`. Error texts: syntax,
  unknown keys and wrong types come from serde with file, line and key snippet; semantic errors
  are `Invalid config file '<path>': <key path + reason>`; the `server.agent_token` migration
  hint is kept. New tests: unknown managed-agent key, duplicate managed names, enum vocabulary.
- Item 4 (services, `crates/redoor/src/service/`): `ServiceBackend` trait (install, uninstall,
  ensure_installed, start/stop/restart/enable/disable, status, render_definition) with
  `SystemdBackend` and `LaunchdBackend`; one dispatcher, one install help text, one
  `ServiceState { installed, enabled, runtime: ServiceRuntime }` formatter (status lines are
  unchanged), one not-installed message builder. Root systemd installs keep the `redoor` system
  user, chown and log directory; launchd keeps the Local Network re-signing repair. Both backends
  compile on Linux and macOS. `redoor_core::subprocess::run_checked(program, args, verbose)` and
  `redoor_core::paths::home()` replace the per-module copies (also used by trash and the agent).
- Item 5 (runtime context): `redoor_core::runtime::RuntimeContext { paths: AppPaths, ws_timeouts }`
  built once in `main`; `AppPaths` (app name, home, cache root, root flag) owns every
  per-application path (data, cache, default config, log dir, process/relay logs) and
  `AppPaths::for_home` isolates tests. Removed globals: `APP_NAME` (and its silent env fallback;
  clap's `env` is the only source), websocket `TIMEOUTS` (`WebSocketTimeouts::keepalive_interval`
  is a method; server handlers/session and the agent profile carry the timings),
  `AGENT_LOADED_CONFIG_PATH` (now `AgentServices::loaded_config_path`), `BINARY_PROVISION_LOCK`
  (`BinaryCache` owns its lock and is shared by upgrades and SSH via `ProvisionContext`),
  `LAUNCH_PARENT` (`LaunchParent::capture()` at the top of `main`, passed to the roles; the
  duplicate `record_launch_parent()` in the relay path is gone). Managed agent registration is
  `ManagedLauncher` (in `ServerState`). The `log!` macros write through a replaceable
  `LoggerHandle` (`logging::install`); records logged before any sink go to stderr, so the ~50
  `logging::init(None)` calls in unit tests and the "OnceLock cannot reset" helper are gone.
- Item 6 (watchdog): `SpawnFn` returns `Result<Child, SpawnError>` (`Provision(ProvisionError) |
  Spawn(io::Error)`, `user_message()` keeps the browser text identical); bounded supervisor
  mailbox of 16 — `start`/`shutdown`/`retry_startup`/`mark_connected` reserve a permit before
  taking the snapshot lock so generation bumps stay atomic, the lifecycle events became `async`
  and wait instead of dropping; `remove_stopped` sends `Remove` after releasing the registry
  lock. `redoor_core::bounded_tail::read_bounded_tail(reader, limit)` replaces both copies.
- Item 7 (SSH): `ssh/prepare.rs` (`PreparedSshBackedAgent`, `RandomRemotePort`, and
  `Preparation { config, destination, relay options, provision, status }` whose `run` is split
  into `ssh_host`, `install_binary` → `force_upload` / `trust_operator_binary` /
  `sniff_and_ensure`, and `run_options`); `ssh/relay.rs` (`run_relay` validation + `Relay` with
  `prepare_until_ready`, `run_attempt` → `AttemptOutcome`, and a testable `Backoff`). Askpass
  re-entry is unchanged.
- Item 8 (dependencies): `fs2` → std `File::lock` (toolchain 1.97; the CLI already locked in
  `spawn_blocking`, MCP tokens in a blocking task). `rand_core`/`OsRng` → `getrandom` 0.4 (already
  pulled by uuid) for secrets and argon2 salts (`SaltString::encode_b64`), so argon2 runs without
  `rand`, core no longer depends on argon2, and getrandom 0.2 left the tree. core/agent use `url`
  instead of reqwest; reqwest without `charset`. `tokio-tungstenite` 0.29 and `tower-http` 0.6
  (the versions axum/reqwest use) and `sha2` 0.11 (rust-embed's) removed the duplicate
  tungstenite, sha1, sha2, rand and tower-http copies. tokio is no longer `full`: each crate lists
  its features, only the binary enables `rt-multi-thread`. TLS stays rustls + aws-lc-rs only.
  Android and macOS type-check clean with the Phase 2 stub-toolchain recipe.
- Deviations:
  - Legacy keys (`agent.ws_address`, `dir`) are separate raw fields rather than `serde(alias)`:
    an alias would turn "both set" into serde's generic duplicate-field error instead of the
    explicit "cannot both be set; use ..." message.
  - Stricter config: `[[agents]]` rows now reject unknown keys and wrongly typed values (they were
    silently ignored), and duplicate effective managed-agent names fail at parse time, so the
    restart pre-validation catches them too.
  - Install/uninstall messages use the shared description (`Installed Systemd unit '…' at …`,
    `Uninstalled LaunchAgent '…'`); status lines and not-installed errors are unchanged.
  - The `watchdog` module is crate-private; `ProvisionError` is `pub(crate)`.
  - `process_control` keeps its `libc::flock` PID-file lock (not fs2); converting the tokio
    handle to std and back around every lock buys nothing.
- Remaining globals and why: the `log!` sink slot and `LOG_LEVEL` (macros are used everywhere;
  the sink is injectable), `identity::EXTERNAL_IP` (process-lifetime cache of a network lookup),
  `safe_fs::EFFECTIVE_USER_HOME` (passwd lookup of an identity that cannot change),
  `ownership::NSS_LOCK` (NSS calls are not thread-safe process-wide), the agent's static
  move-cleanup semaphores (process-wide bound on background deletions). Intentional
  `process::exit`s remain only for a failed re-exec and for the parent-death hook (an orphan must
  not keep running and has nothing to unwind into).
- Tests: full `pn test` green on the first run, no flakes (integration 179 s, Playwright 312 s).
  `REDOOR_SSH_TEST=1 pn integration-test tests/real-ssh.test.ts` also passed (relays and managed
  SSH; the password-only cases skip without `REDOOR_SSH_TEST_PASSWORD`).
- Remaining `Result<_, String>`: clap value parsers and `FromStr` impls (clap's convention), and
  agent command internals (download sources, platform mounts/accounts) whose string is the wire
  error message.
