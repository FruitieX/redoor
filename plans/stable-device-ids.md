# Stable device ids

## Goal

Every device gets an opaque id that never changes for its lifetime. The display name stays
editable and is no longer used as the identity.

## Why

Today the agent id is the name. The agent sends `agent_id: AgentId::from(settings.name)`
in `hello` (`crates/redoor-agent/src/lib.rs`). Managed agents get the name from the
`[[agents]]` entry (`name`, or the host part of `target`) and pass it with `--name`
(`crates/redoor-server/src/ssh/prepare.rs`). Standalone agents get it from their own
config.

That id is persisted in places that do not follow a rename:

- **Token scopes.** `DeviceScope::Only { device_ids }`
  (`crates/redoor-protocol/src/api/tokens.rs`) is stored in `access-tokens.json`, and
  renaming a device in `http/agent_configuration.rs` does not touch it.
  - A token scoped to `pi` silently loses access when `pi` is renamed.
  - If a different device is later added as, or renamed to, `pi`, the token gains access
    to it. This is a privilege escalation, not only an inconvenience.
- **Resume manifests** (`source_agent_id`, `dest_agent_id` in
  `crates/redoor-core/src/partials.rs`).
- **Anything later phases add per device**, such as the SSH key directories in phase 1.

## Design

### Id format

- `dev_` followed by 20 random lowercase base32 characters (100 bits). It is safe as a
  path component, a URL segment and a TOML string.
- The `dev_` prefix is only there for readability: an id is recognisable as a device id in
  logs, `config.toml`, token scopes and URLs, like the existing `redoor_at_` prefix on
  access tokens. It places no restriction on names.
- `hello` must carry a valid id. A `hello` whose `agent_id` is not in this format is
  rejected.

### No backwards compatibility

Nothing from earlier versions is migrated or accepted, and no code is written for it.

### Who owns the id

The owner depends on whether the server has a config entry for the device.

**Managed agents (`[[agents]]`, SSH and local): the server owns the id.**

- Stored as `id = "dev_..."` in the `[[agents]]` entry and created when the device is
  added through the UI or API.
- At startup, entries without an `id` get one, written back into `config.toml` with the
  same formatting-preserving rewrite that agent edits already use. This is for
  hand-written entries, not for compatibility: users write `[[agents]]` entries by hand
  and should not have to invent an id. The id is stable from then on.
- **The config cannot be written** (mounted read-only in Docker, owned by another user,
  ...): startup fails with a clear error that names the entries without an id and prints a
  freshly generated `id = "dev_..."` line for each, ready to paste. An id kept only in
  memory is not an option, because it would change on every start and silently break
  token scopes and, in phase 1, SSH keys.
- **Ids must be unique.** Copying an `[[agents]]` block copies its `id`, which would
  silently merge two devices (and in phase 1 make them share one key directory). Startup
  and the pre-restart config validation reject duplicate ids, naming both entries, next to
  the existing `ensure_unique_agent_names` check. The same applies to `[[relays]]`.
- **Edits never change the id.** The SSH and local agent edit handlers in
  `http/agent_configuration.rs` (`update_ssh_agent` and its local counterpart) rebuild the
  whole entry from the request and only carry `password` over from the existing entry.
  They must also carry `id` over. The id is never part of a create or edit request body; the
  server creates it on add and copies it on edit. Without this, any edit, including a
  rename, would drop the id, and the next startup would assign a new one.
- The server passes the id to the agent it spawns with a new `--agent-id` argument (next
  to `--name`). An agent started with `--agent-id` uses it and never reads or writes the
  id file. The id is not secret, so having it on the command line is fine.
- Why the server owns these instead of the agent:
  - Several `[[agents]]` entries can target the same `user@host` (that is why managed
    agents skip the singleton lock). They share one data dir, so an id file there would
    give them all the same id.
  - The server needs the id before the first connection: to tie the entry to its
    supervisor, to offer it in token scopes and, in phase 1, to set up the device's SSH
    key.
  - Wiped remote state (reinstall, cleared Termux data, `$HOME` on tmpfs, a fresh
    container) does not change the id.

**Standalone agents: the agent owns the id.**

- On first start the agent creates an id and stores it in `<data_dir>/agent-id` (0600,
  written atomically). Later starts reuse it and send it in `hello`.
- The server has no config entry for these devices, so the agent is the only place that
  can remember the id.
- Deleting the file makes the agent appear as a new device. That is the documented way to
  give a cloned machine its own identity.

**Relays (`[[relays]]`, `redoor agent relay`).** The relay launches an agent for a remote
server over SSH, the same way as managed agents. The relay entry gets an `agent_id`,
created like the `[[agents]]` id, and passes it with `--agent-id`.

### Cloned standalone agents

Copying a home directory, a VM image or a container image with a baked-in data dir copies
the id file, so several machines report the same id. Today a reconnect of the same id
replaces the old connection, so the clones would keep evicting each other.

Comparing hostnames is not enough: clones usually keep the same hostname and username, so
they would look like ordinary reconnects. The router detects the eviction loop itself,
which works on every platform and needs nothing from the OS:

- The router counts, per id, how often a new `hello` replaces a connection that was still
  healthy (its heartbeat was fresh). A real reconnect replaces a dead or dying connection,
  and does so once. Clones replace each other's healthy connections over and over.
- When an id has 3 such replacements within 60 seconds, the router stops replacing: while
  the current connection stays healthy, every newcomer for that id is rejected with a new
  `duplicate_id` rejection reason. The device shows a warning ("several machines are using
  this device's id"), including the hostnames reported in the competing `hello` frames.
- A rejected agent logs how to fix it (delete `<data_dir>/agent-id` on one of the machines)
  and retries with its normal backoff, so a fixed clone or a real reconnect after the
  current connection dies gets through.
- The warning clears after the window passes without new replacements.
- Managed agents get their id from their own `[[agents]]` entry, and duplicate ids there
  are rejected at startup, so this case only affects standalone agents in practice. The
  check still applies to every connection.
- This trusts self-reported behavior, like every agent check. Proper per-agent credentials
  are a separate topic.

### Names

- The name is for people: it is shown in the UI and used to address a device in URLs, the
  REST API, MCP and the CLI. It is never stored as a reference; everything stored uses the
  id.
- Name format rules, checked wherever a name enters the system (`[[agents]]` validation at
  startup, device create and rename requests, and `hello` `agent_name`):
  - not empty,
  - no `:` or `/`: the CLI's `device:path` syntax splits at the first `:` and rejects `/`
    in the device part (`crates/redoor-cli/src/copy.rs`), and `/` would break URL paths,
  - not in the id format (`dev_` followed by 20 base32 characters): requests accept a
    name or an id, so a name that looks exactly like an id would be ambiguous. Other names
    starting with `dev_` are fine.
- Names must be unique, so a name in a request always points to exactly one device, and
  two devices called `pi` are not confused in the UI. Uniqueness is not a security
  property, because access control works on ids (see "Ids and names in APIs"). The
  existing checks stay: duplicate names in `[[agents]]` fail startup
  (`ensure_unique_agent_names`), creating or renaming a device to a taken name is refused,
  and a different id that claims a live agent's name is rejected.
- Renaming a device changes only the name. Every stored reference uses the id, so nothing
  else has to be rewritten.
- The `hello` frame already has separate `agent_id` and `agent_name` fields, so the wire
  format does not change. Only their values become different.

### Ids and names in APIs

- **Stored and internal references use ids only**: router keys, token scopes, resume
  manifests, MCP operations, config and anything later phases add per device.
- **Requests accept a name or an id** wherever they address a device: `{agent}` in REST
  paths, device fields in request bodies (copy, move, diff, ...), MCP tool arguments, and
  the CLI's `device:path`.
  - An exact id match wins, otherwise the name is matched. Names cannot be in the id
    format, so this is never ambiguous.
  - Addressing by name is intentional: a script that uses `pi` follows whichever device is
    called `pi` now, so a device can be swapped out by renaming. Scripts that must stay on
    one device use its id.
- **Resolution happens once, before access control.** The access middleware resolves
  `{agent}` to an id before `Principal::require` runs, and handlers resolve body fields the
  same way before calling `require`. Scope checks therefore always compare ids. A token
  scoped to device X that addresses `pi` after `pi` has become a different device gets
  403, not access to the new device.
- **Token scopes resolve at creation.** Creating or editing a token with device names in
  its scope stores their ids. A token's scope stays tied to the devices it was created
  for and does not follow names.
- Every device response includes both `id` and `name`, through `#[ts(export)]` structs.
  Run `scripts/generate-ts-bindings` after changing them.
- **CLI**: passes the device value straight to the server. If the server answers "device
  not found", the CLI lists the available devices with their names and ids.
- **UI URLs use names** (`/agents/pi/browser/...`), because they are read and typed by
  people. Names are unique and cannot contain `/`, so a name in a URL always points to
  exactly one device. Unlike scripts, an open page must not switch devices under the user,
  so the UI uses the name only to find the device and everything after that uses the id:
  - Rename the route parameter from `$agentId` to `$deviceName`
    (`ui/src/routes/_app/agents.$deviceName.*`), so code cannot mistake the name for an
    id.
  - The `agents.$deviceName` route loader resolves the name to the id once, from the
    device list in the Query cache, and provides the device (id and name) to child
    routes. Every API call, terminal tab, websocket and piece of stored UI state
    (`localStorage`, the device picker, the bottom drawer) uses that id, never the URL
    parameter. A page left open across a rename, or across a delete and re-add with the
    same name, therefore keeps acting on the device it was opened for.
  - An unknown name shows a "device not found" page with a link to the device list. This
    also covers an offline standalone agent that the server does not know about yet after
    a restart.
  - Renaming a device from its own page moves the current tab to the new URL (replacing
    the history entry). Other open tabs, bookmarks and shared links with the old name show
    "device not found", or open whichever device later takes that name. This is accepted:
    a browser session can reach every device anyway, so it is a surprise, not a security
    issue.
  - Names may contain spaces, `?`, `#`, `%` and Unicode. Links go through TanStack Router
    (`Link` and `navigate` with `params`), which encodes them. Never build device URLs
    by string concatenation.

## Implementation steps

1. `AgentId` constructor for new ids, format validation, and rejection of a `hello`
   without a valid id. Name validation (not empty, no `:` or `/`).
2. Config: `id` on `[[agents]]`, `agent_id` on `[[relays]]`, assignment and write-back at
   startup, creation when a device is added. The read-only config error with ready-to-paste
   ids. Duplicate id validation at startup and before restarts.
3. Edit handlers in `http/agent_configuration.rs` copy `id` from the existing entry.
4. Agent: the `--agent-id` argument, the id file for standalone agents, and using the id in
   `AgentProfile` instead of the name.
5. Server: pass `--agent-id` from the SSH launcher, the local agent spawner and the relay.
   Key supervisors by id.
6. Router: eviction-loop detection with the `duplicate_id` rejection and device warning,
   and keeping the name-uniqueness rule.
7. Name-or-id resolution: in the access middleware for `{agent}`, in handlers for device
   fields in request bodies, in MCP tool arguments, and in token creation and editing
   (stored as ids). All of it runs before `Principal::require`. The CLI lists devices on
   "device not found".
8. Device responses with `id` and `name` and TS bindings. UI: rename the route parameter to
   `$deviceName`, resolve it in the loader, switch API calls and stored UI state to the
   resolved id, add the "device not found" page and the rename redirect. Run
   `cd ui && pnpm run build` to regenerate the route types.
9. Test fixtures: `hello` now rejects ids that are not in the `dev_` format, so the fake
   agent (`http/fake_agent.rs`), the router unit tests (which use ids such as
   `AgentId::from("a")`), the integration test helpers in `tests/` and the dev agents
   config all need valid ids. Add one shared helper per language that creates a valid id,
   instead of hard-coding id strings in each test.
10. Update `ARCHITECTURE.md`: agent identity (id vs name and who owns the id), the `hello`
    description, the duplicate-id handling, the access control section on device scopes,
    and the trust note about self-reported ids.

## Tests

- **Rust unit tests**:
  - id generation format and validation,
  - name-or-id resolution: an exact id wins, then the name; an unknown value is "device
    not found",
  - config write-back keeps the file's formatting and comments,
  - duplicate ids in `[[agents]]` or `[[relays]]` fail validation and name both entries,
  - names that are empty, contain `:` or `/`, or are in the id format are rejected; other
    names starting with `dev_` are accepted,
  - a `hello` with a missing or malformed id is rejected,
  - eviction-loop detection: a single replacement of a dead connection is a normal
    reconnect; 3 replacements of healthy connections within 60 seconds turn into
    `duplicate_id` rejections; the warning clears after a quiet window.
- **Integration**:
  - renaming a device keeps its id, and a token scoped to it still works after the rename,
  - editing any other field of an SSH or local device keeps its id in `config.toml`,
  - startup with a read-only `config.toml` that has an entry without an id fails with the
    error that prints a ready-to-paste id,
  - a new device that takes a renamed device's old name is not visible to a token scoped
    to the old device (the escalation this phase fixes),
  - a standalone agent keeps its id across restarts, and gets a new one after the id file
    is deleted,
  - two `[[agents]]` entries on the same `user@host` connect at the same time with
    different ids,
  - an SSH device keeps its id after its remote data dir is deleted,
  - two standalone agents with the same id file and the same hostname (a clone) end up
    with one connected and the other rejected with `duplicate_id`, and the warning is
    shown; they do not keep evicting each other,
  - restarting a standalone agent (a normal reconnect) is never rejected,
  - REST paths, request body device fields, MCP tools and CLI `device:path` work with both
    a name and an id,
  - a script addressing `pi` reaches the new device after the old `pi` is renamed and
    another device takes the name,
  - a token scoped to device X gets 403 when it addresses X's old name after another
    device has taken it,
  - creating a token with a device name in its scope stores the id, and the scope does
    not follow the name after a rename,
  - creating or renaming a device to a name that is already taken is refused,
  - a `config.toml` without ids gets ids at startup, and they stay the same across
    restarts.
- **Playwright**:
  - rename a device from its page: the URL changes to the new name and open terminal tabs
    keep working,
  - a page opened before a rename keeps acting on the same device (its API calls use the
    id),
  - a name with spaces, `#`, `?` and `%` works in URLs, links and the browser address bar,
  - an unknown name shows the "device not found" page.

## Open questions

- Should the UI show the id anywhere (for example in the device details, with
  `CopyableCodeRow`)? Scripts that must stay on one device across renames need it.
  Proposal: yes, in the device details only.
