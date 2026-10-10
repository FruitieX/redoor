# Agent wire protocol v2

Redesign the server <-> agent websocket protocol now that there are no backwards
compatibility requirements. The current protocol (described in `ARCHITECTURE.md`) was kept
byte-compatible during the workspace rewrite, which left redundant fields, a fallback code
path, four socket types and cross-socket ordering problems in place.

Only the agent-facing protocol changes. The REST API, the browser websockets
(`/api/v1/ui/ws`, browser terminal and log sockets), the MCP endpoint and `bindings/` used by
the UI keep their current shape unless a step below says otherwise.

## Goals

1. No `agent_id` in agent frames: identity comes only from the authenticated connection.
2. Authenticate during the websocket upgrade, so unauthenticated sockets never exist.
3. Explicit protocol version negotiated during the upgrade.
4. Per-stream credit is mandatory; the uncredited fallback is deleted.
5. One multiplexed connection per agent instead of control, transfer, terminal and log
   sockets.
6. One ordered lifecycle per request: data, then exactly one result, on one ordered stream.
7. A cancel can never arrive before its command; no early-cancel tombstones.
8. Responses typed by their command instead of one shared `CommandResult` enum.
9. Request ids scoped to one connection.

Non-goals: changing control frames away from JSON, changing the 64 KiB payload frame size,
changing the REST API.

## Compatibility stance

- Agents and servers of different protocol versions refuse to talk (see "Handshake"). There is
  no compatibility path for v1 agents.
- Managed local and SSH agents are unaffected in practice: the server provisions its own
  binary to them (SHA-1 / version check in `ssh/provision.rs`) before launching.
- Standalone agents (systemd/launchd/manual) of an older version must be upgraded manually
  (`install.sh` or replacing the binary). The server's "upgrade agent" REST action only works
  between agents that already speak the same protocol version. The UI should show
  version-mismatched connection attempts clearly (see "Handshake").
- Tests that act as fake agents (`tests/agents.test.ts`, `tests/agent-upgrade.test.ts`) are
  rewritten against a shared TypeScript fake-agent helper.

## Target design

### Endpoint and handshake (goals 2, 3)

One agent endpoint, `GET /api/v1/agent/ws` (replaces `/ws`, `/api/v1/agent-transfer/ws`,
`/api/v1/terminals/{id}/agent/ws`, `/api/v1/log-streams/{id}/agent/ws`).

Upgrade request headers sent by the agent:

- `Authorization: Bearer <agent token>`
- `Sec-WebSocket-Protocol: redoor.v2`
- `X-Redoor-Agent-Version: <binary version>` (diagnostics only)

The server validates both before accepting the upgrade:

| Condition | Response |
|---|---|
| Missing or wrong token | `401` before upgrade; no socket is created |
| Subprotocol not offered or unknown | `426 Upgrade Required` with a JSON body naming the server's protocol and version |
| Valid | `101`, echoing `Sec-WebSocket-Protocol: redoor.v2` |

The agent treats `401` as fatal (wrong token) and `426` as fatal with a clear log line
("server speaks redoor.vN, this agent speaks redoor.v2; upgrade the agent"), not as a
reconnect loop. The server logs rejected attempts with remote address and the agent version
header, and keeps a bounded list of recent mismatched attempts that the UI can show (REST
type with `#[ts(export)]`).

After `101`, the first frame from the agent is:

```json
{"type":"hello","agent_id":"...","agent_name":"...","os":"...","arch":"...", ...identity, "capabilities":{...}}
```

The server answers with exactly one of:

```json
{"type":"welcome","connection_id":"..."}
{"type":"rejected","reason":"duplicate_name","message":"..."}
```

`hello` is the only frame that carries `agent_id`; from then on the connection *is* the
agent. The SSH token delivery (stdin → `REDOOR_AGENT_TOKEN` env) is unchanged; the token just
moves from the JSON frame into the upgrade header. Keepalive pings stay as they are.

### Framing on the multiplexed connection (goal 5)

- **Text frames**: JSON control messages, `ServerToAgent` / `AgentToServer`, no `agent_id`
  fields anywhere (goal 1).
- **Binary frames**: stream data. Header: `stream_id: u64`, `flags: u8` (`FIN`), then up to
  64 KiB of payload. Replaces the current `StreamChunk` header; the request id *is* the
  stream id for request streams.

Stream kinds, all identified by a per-connection `u64` id allocated by the side that opens
them:

| Kind | Opened by | Data direction |
|---|---|---|
| Request output (download, exec output, copy source, diff, file read) | server, with the command | agent → server |
| Request input (upload, edit, archive upload, copy destination) | server, with the command | server → agent |
| Terminal | server (`terminal_open`) | both |
| Log stream | server (`log_stream_open`) | agent → server |

Ids are allocated by the server only, so there is one id space per connection (goal 9).

### Scheduling (keeps control responsive on one socket)

Each side has exactly one writer task with three classes, drained in strict order:

1. **Priority control**: `cancel`, `credit`, `publish_decision`, `welcome`/`rejected`,
   terminal resize/close. Never queued behind data.
2. **Interactive streams**: terminal data. Small frames, latency-sensitive.
3. **Bulk streams**: everything else, round-robin across streams that have queued data
   *and* credit.

Because no payload frame exceeds 64 KiB, a control frame waits for at most one bulk frame.
Stream-scoped control frames (`result`, `publish_request`) are **not** priority: they are
queued in their stream's queue so they are written after that stream's data (see "Request
lifecycle").

Risk to measure: one TCP connection means one congestion window and TCP head-of-line
blocking, where separate sockets previously had independent windows. Keep
`tests/raw-upload-toxiproxy.test.ts` and add a terminal-latency test (below) to make the
trade-off visible.

### Credit (goal 4)

- Every stream in every direction is credited. There is no capability flag and no fallback.
- The **receiver grants** credit with `{"type":"credit","stream":N,"frames":K}` (priority
  class). A sender may have at most the granted number of non-`FIN` frames outstanding.
- Initial credit:
  - Agent → server streams: the command frame carries `"credit": 4`, so output starts without
    a round trip.
  - Server → agent streams: initial credit is 0. The agent grants credit when its sink is
    ready. **This first grant replaces `transfer_ready`.**
  - Terminal streams: generous fixed window in both directions (e.g. 16 frames), granted in
    `terminal_open` / its acknowledgement.
- `FIN` frames and stream-scoped results spend no credit.
- A peer exceeding credit fails only that stream (`result` with `ProtocolViolation` / the
  stream is reset); the connection continues.
- Memory bound: per connection ≤ (active streams × window × 64 KiB). Active request streams
  are already capped (`MAX_CONCURRENT_COMMANDS = 32` on the agent); add explicit caps for
  terminals and log streams and document the resulting worst case in `ARCHITECTURE.md`.

### Request lifecycle (goals 6, 7)

Every request is `command` → optional data frames → exactly one `result`, all ordered on the
connection:

```
server: {"type":"command","id":7,"credit":4,"command":{...}}
agent:  binary(stream 7) ... binary(stream 7, FIN)
agent:  {"type":"result","id":7,"ok":{...}}        or  {"type":"result","id":7,"error":{"kind":"...","message":"..."}}
```

Rules:

- **Exactly one `result` per command id**, always last. It is queued behind the stream's own
  data, so the receiver sees all data (and `FIN`) before the result. The server's
  "stream ended vs. command responded" reconciliation phases go away.
- Uploads: command (credit 0) → agent grants credit when ready → data → `FIN` →
  agent `publish_request` (stream-ordered) → server `publish_decision {allowed}` →
  agent publishes or discards → `result`. The publication round trip stays because it is what
  settles a cancel racing a publish.
- **Cancel**: `{"type":"cancel","id":7}` on the priority class. The server only sends it for a
  command whose frame has already been written to the socket; a command still in the outbound
  queue is removed locally and completed as canceled without any wire traffic. Since the
  command frame was written before the cancel on the same ordered connection, the agent
  always knows the id when the cancel arrives → delete `pending_cancellations` and the
  1024-entry tombstone cap.
- A cancel is acknowledged by the command's normal `result` (kind `Canceled` if it was
  canceled, or the real result if it had already finished). Cancels for ids the agent has
  already answered are ignored (no state kept).
- Exec: the exit status is part of exec's typed `result`, which is ordered after the output
  `FIN`. Unread output can still never block it because the result spends no credit; the
  server keeps delivering the exit status to the HTTP layer independently of the body (as
  today with `watch`).
- Progress (`transfer_progress`) stays a stream-scoped control frame; counts for streamed
  bytes continue to be derived from frames.

### Typed responses (goal 8)

- In `redoor-protocol`, each command gets a request struct implementing
  `trait AgentRequest { type Response: Serialize + DeserializeOwned + TS; }`.
- `Command` remains the enum used for dispatch (`#[serde(tag = "type")]`), but there is no
  `CommandResult` enum. The `result.ok` payload is the command's `Response` type.
- Server: `AgentCall` becomes generic over the request type; the pending-request table stores a
  boxed decoder or the raw JSON value, and `AgentClient::call` deserializes into
  `C::Response`. All "unexpected result type" branches disappear.
- Agent: each `RunCommand` implementation returns its own `Response`; the dispatcher
  serializes it into `result.ok`.
- Error shape stays `{kind, message}` with `CommandErrorKind`.
- REST DTOs that today wrap `CommandResult` variants keep their current JSON (REST is out of
  scope), built from the typed responses.

### Removed concepts

- `/ws`, `/api/v1/agent-transfer/ws`, `/api/v1/terminals/{id}/agent/ws`,
  `/api/v1/log-streams/{id}/agent/ws` agent endpoints.
- `transfer_socket_open`, `transfer_ready`, `agent_unregister` (closing the socket is enough),
  `supports_stream_credit`, `agent_register`.
- Transfer socket tokens, terminal and log one-time tokens for the agent side.
- `TransferLink` on the agent; the transfer-link generation; server socket-id guards for the
  transfer socket (the connection id remains for the single socket).
- Early-cancel tombstones.
- `CommandResult`.

## Implementation phases

Each phase ends with `mise exec -- pn test` passing, plus
`REDOOR_SSH_TEST=1 REDOOR_SSH_TEST_PASSWORD=... pn integration-test tests/real-ssh.test.ts`,
and logical commits on a branch. Follow `AGENTS.md` (why-comments, tokio async APIs, bounded
memory, no sleeps in tests, `tokio::select!` arms delegate, regenerate bindings).

Every phase must also update [ARCHITECTURE.md](../ARCHITECTURE.md) when it changes anything the
document describes (endpoints, sockets, frames, handshake, credit, lanes, request lifecycle,
crate responsibilities), including the affected Mermaid diagrams and their explanations. The
document must match the code at the end of each phase, not only after phase 5.

### Phase 1: Handshake, identity and mandatory credit (goals 1, 2, 3, 4, 9)

Still on the existing socket layout, so the change stays reviewable.

- New `/api/v1/agent/ws` with upgrade-time auth and `redoor.v2` subprotocol; `hello` /
  `welcome` / `rejected`. Remove `/ws`.
- Transfer socket also authenticates at upgrade (temporary; it disappears in phase 2).
- Remove `agent_id` from all post-handshake frames on both sides; the session supplies it.
- Per-connection request ids allocated by the server.
- Remove `supports_stream_credit` and every uncredited code path on server and agent.
- Agent: `401`/`426` handling with clear, non-looping errors. Server: mismatched-attempt list
  + REST endpoint + small UI surface (Playwright test).
- Add `tests/fake-agent.ts` (handshake, command/result helpers, binary framing) and rewrite
  `tests/agents.test.ts` and `tests/agent-upgrade.test.ts` on it.
- Tests: wrong token → 401 with no session created; missing/unknown subprotocol → 426;
  duplicate name → `rejected`; frames before `hello` close the socket.

### Phase 2: Multiplexed connection (goal 5)

- New binary frame header (`stream_id`, `FIN`); one writer per side with the
  priority / interactive / bulk scheduler; one reader that demuxes into the existing stream
  tables (`StreamLease`, `CopyForwarder` keep their roles).
- Fold the transfer socket into the connection; delete `transfer_socket_open`, transfer
  tokens, `TransferLink`, and the "transfer lost but control alive" handling.
- Move terminals and log streams onto mux streams. The browser-side sockets and the server
  bridge stay; the bridge now pairs a browser socket with a stream instead of a second agent
  socket. Keep the bounded pre-ready input buffer.
- Explicit caps for terminal and log streams per connection.
- Tests: stalled download does not delay a terminal echo or an exec on the same agent
  (latency assertion via polling, no sleeps); control command round trip stays fast during a
  bulk upload; credit violation on one stream leaves others working; the toxiproxy upload
  test still passes.

### Phase 3: Ordered request lifecycle and cancel (goals 6, 7)

- Stream-scoped `result` and `publish_request` queued behind stream data; exactly-one-result
  invariant enforced on the agent (debug assertion + test) and relied on by the router.
- Credit grant replaces `transfer_ready`; command frames carry initial credit for output
  streams.
- Server sends `cancel` only for written commands; unsent commands are dequeued locally.
  Delete agent tombstones.
- Simplify router flow phases that reconciled stream end vs. response (`flows.rs`,
  `streams.rs`, copy/upload transfers).
- Tests: result never precedes its data; cancel racing completion yields exactly one result;
  cancel of a queued-but-unsent command produces no wire frame; upload cancel during
  publication still never replaces the destination.

### Phase 4: Typed responses (goal 8)

- `AgentRequest` trait and per-command `Response` types in `redoor-protocol`; delete
  `CommandResult`.
- Generic `AgentCall` / `AgentClient::call`; per-command `RunCommand` returns its response.
- Keep REST DTO JSON unchanged; regenerate bindings and confirm only agent-protocol types
  changed.
- Tests: decoding of every response type (round trip per command), error results map to the
  same `ApiError` statuses as before.

### Phase 5: Documentation and review

- Review [ARCHITECTURE.md](../ARCHITECTURE.md) end to end against the final code: the single
  connection, the scheduler classes, credit grants and the request lifecycle. Fix anything
  the per-phase updates missed.
- Final review subagent over the whole diff (correctness, memory bounds, responsiveness,
  `AGENTS.md`), then fixes, full `pn test` and the real SSH suite.

## Open questions

- Window sizes: 4 × 64 KiB per bulk stream matches today; terminal window to be tuned with
  the latency test.
- Whether log streams should be interactive or bulk class (default: bulk, they tolerate
  latency).
- Whether to surface version-mismatch attempts in the UI or only in server logs (plan
  assumes a small UI surface).
