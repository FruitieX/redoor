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
