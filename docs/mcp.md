# MCP

Redoor exposes an authenticated, stateless MCP endpoint at `POST /mcp`. The
implementation targets only the published `2026-07-28` protocol and provides
`server/discover`, `tools/list`, and authenticated device tools. Every
request is independent: there is no `initialize` handshake, MCP session ID,
standalone GET event stream, resumable SSE, or client capability remembered
from an earlier request.

## Create a machine credential

The normal setup path is **MCP tokens** in Redoor's application menu. Select
only the devices and permissions the client needs, create the token, and copy
both the endpoint and bearer token from the one-time result. Read-only device
inventory and file access are selected by default; no device is selected by
default. Shell access is deliberately separate because a command runs with
the agent OS user's authority and can bypass file-tool scope restrictions.

The settings page never saves the displayed secret in browser storage or its
metadata cache. Closing the result discards the browser's copy. Rotation
preserves the stable grant and immediately invalidates the prior credential;
revocation immediately rejects later MCP and prepared streaming requests.

The administrative CLI remains available for bootstrap and headless servers.

Machine credentials live under the selected application's private data
directory. They are separate from the browser password, browser cookies, and
agent registration token. Create one with:

```sh
mise exec -- redoor --app-name redoor server mcp-token create --name "My MCP client"
```

The JSON output contains `token` once. Redoor stores only its verifier. Save
the token in the client's secret store; it cannot be recovered later.

The policy model already supports the scopes `devices:read`, `files:read`,
`files:write`, `files:delete`, `commands:execute`, and `agents:manage`, plus an
explicit device allowlist. The `hello` tool needs neither a scope nor a device.
For example, a device-reading credential can be bootstrapped with:

```sh
mise exec -- redoor --app-name redoor server mcp-token create \
  --name "Read-only automation" \
  --scopes devices:read,files:read \
  --devices DEVICE_ID \
  --expires-at 2027-01-01T00:00:00Z
```

List secret-free metadata or revoke a credential by its ID:

```sh
mise exec -- redoor --app-name redoor server mcp-token list
mise exec -- redoor --app-name redoor server mcp-token revoke CREDENTIAL_ID
```

CLI updates are serialized across processes and atomically published. A
running server re-reads the current grant and credential on every request, so
expiry and revocation take effect without a restart.

## Client configuration

Configure an HTTP MCP client with the endpoint and bearer header:

```json
{
    "servers": {
        "redoor": {
            "url": "http://127.0.0.1:7666/mcp",
            "headers": {
                "Authorization": "Bearer YOUR_ONE_TIME_TOKEN"
            }
        }
    }
}
```

This generic shape applies only to clients that implement MCP `2026-07-28`.
Each POST must include:

- `MCP-Protocol-Version: 2026-07-28`
- `Mcp-Method` matching the JSON-RPC method
- `Mcp-Name` matching `params.name` for `tools/call`, using the specification's
  Base64 sentinel encoding when it is not safe plain ASCII
- `params._meta["io.modelcontextprotocol/protocolVersion"]`
- `params._meta["io.modelcontextprotocol/clientCapabilities"]`, which may be
  an empty object

Client identity in `_meta` is optional diagnostic information and never
changes authorization. A direct first request may be `tools/call`; discovery
is optional.

The official TypeScript and Rust MCP SDK releases reviewed for this work do
not yet expose the final `2026-07-28` stateless wire contract. In particular,
their older "stateless HTTP" modes still use initialization-era messages and
do not establish support for per-request metadata or `Mcp-Method`/`Mcp-Name`.
The integration tests therefore use a small revision-specific HTTP helper in
`tests/mcp-client.ts`. Initialization-era clients, including clients that open
MCP with GET or require an `Mcp-Session-Id`, are not compatible.

## Network and Origin security

Redoor binds to loopback by default. Keep the loopback URL for a client on the
same machine. Remote deployments must terminate HTTPS before exposing MCP;
bearer tokens must not cross an unencrypted remote network.

Origin-less native clients are accepted. Browser clients must have their exact
origin configured:

```toml
[server]
mcp_allowed_origins = ["https://trusted-client.example"]
```

An unconfigured or malformed `Origin` receives 403. MCP has its own narrow
CORS and bearer middleware and does not inherit the wildcard browser API CORS
or the legacy rule that treats non-`/api/` paths as public.

Responses are bounded JSON. This initial server does not emit SSE, advertise
subscriptions/resources/prompts, or initiate calls into a client.

## Device tools and authorization

The catalog contains `hello`, `list_devices`, `get_device`, `run_command`,
`get_operation`, `read_command_output`, `cancel_operation`, `list_directory`,
`stat_path`, `read_file`, `write_file`, `copy_path`, `move_path`,
`delete_path`, `create_directory`, `search_paths`, `search_contents`,
`start_managed_agent`, `stop_managed_agent`, `prepare_file_download`, and
`prepare_file_upload`.

Every filesystem request includes an explicit stable `device_id` and absolute
path. There is no remembered device or working directory. Inventory is
filtered to the grant's device allowlist. File tools require their matching
`files:*` scope, copy requires source read and destination write, and move also
requires source delete. Arbitrary command execution has its own high-trust
`commands:execute` scope because a process runs with the device agent's OS
authority and can bypass the file-tool policy. Managed lifecycle calls require
`agents:manage` and reject external agents.

Background commands and copy/move calls return an operation UUID. Operation
state, output pages, and cancellation are restricted to the stable grant that
admitted the work; rotating a credential can therefore preserve ownership.
Current device and scope permissions are checked again when work is inspected
or canceled. Cancellation acceptance is reported separately from remote
termination acknowledgement. Optional `idempotency_key` values on transfer
admission are principal-scoped and bound to the tool and its arguments; reusing
a key for a different tool or different arguments is rejected. Concurrent
retries share one durable operation UUID. If a keyed admission's remote start
fails, inspect that operation for the failure; retrying the same key returns
the original admission without starting new work. Transfer completion is
persisted independently of client polling. After a server restart, retained
operations cannot cancel work admitted by the new server.

`read_file` and `write_file` are limited to 64 KiB per call. Command retention
defaults to 1 MiB per operation and 64 MiB globally, reports truncation while
continuing to drain the agent stream, and expires after 24 hours. Configure
these with `REDOOR_MCP_OPERATION_JOB_QUOTA`,
`REDOOR_MCP_OPERATION_GLOBAL_QUOTA`, and
`REDOOR_MCP_OPERATION_EXPIRY_SECONDS`. Directory and search results are bounded. Copy and
move keep bytes in Redoor's existing backpressured device transfer pipeline;
they are not embedded in MCP responses.

The prepare tools return a relative endpoint such as
`/mcp/files/DEVICE_ID/absolute/path`. Resolve it against the server origin and
send the same bearer token. Downloads accept HTTP `Range`; uploads require
`Content-Length` and use `?on_existing=error`, `replace`, or `merge`. These
endpoints stream through the existing agent pipeline and reauthenticate the
credential and device grant on every request. Treat prepared URLs and bearer
headers as private even though the URL alone is not sufficient authorization.

```sh
curl -H 'Authorization: Bearer YOUR_TOKEN' \
  -H 'Range: bytes=0-65535' \
  'https://redoor.example/mcp/files/DEVICE_ID/home/user/large.bin'

curl -X PUT -H 'Authorization: Bearer YOUR_TOKEN' \
  -H 'Content-Length: 1048576' --data-binary @local.bin \
  'https://redoor.example/mcp/files/DEVICE_ID/home/user/large.bin?on_existing=error'
```

The deployment remains single-server: live agent routes and transfer progress
are process-local. Operation admissions and retained command output are durable;
a server restart marks work lacking a terminal acknowledgement `interrupted`
and never repeats it. Multiple replicas require shared
operation storage plus routing to the server that owns each device connection.
Filesystem-root confinement is not implemented; stronger confinement must be
canonical, symlink-aware, and enforced by the agent or its OS container.
