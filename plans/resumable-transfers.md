# Resumable file transfers between agents

## Goal

Let a file copy or move, between agents or on the same agent, that was interrupted
(connection lost, agent crashed, source read error) or paused from the UI continue from
where it stopped instead of starting over. Same-agent transfers matter too: on slow
spinning disks a local copy of a large file can take as long as a network transfer.

The partial files on the destination agents are the only source of truth for what can be
resumed. Neither the server nor the agents keep resume state in process memory. Pausing ends
the transfer completely, and resumable transfers are found by asking the agents for their
partials. This makes resume work the same after server restarts, agent restarts and machine
crashes.

Implementation waits until the wire protocol v2 rewrite (`plans/wire-protocol-v2.md`) has
landed. This feature adds new commands and changes stream abort handling, which is exactly
the code that rewrite is reshaping.

## Scope

In scope:

- Cross-agent raw-file copies and moves (`CopyContentKind::RawFile`).
- Same-agent file copies (`Command::LocalCopyFile`).
- Same-agent file moves that fall back to copy and delete (`Command::LocalMove` when the
  agent cannot publish with a rename: different filesystems, or a `Merge` conflict policy).
- Pausing an active resumable copy or move from the UI.
- Keeping the partial file when the transfer is interrupted.
- A list of resumable transfers built from the agents' partials, with Resume and Discard.
- Detecting a resumable partial when the user initiates the same copy again.
- blake3 verification of the partial prefix before the resumed file is published.
- Server-signed manifests, so an agent cannot forge a resumable transfer.
- Automatic cleanup of stale partials.

Out of scope:

- Directory copies (`TarDirectory`). Tar streams are produced on the fly, so a byte offset
  does not identify a stable position in the tree. Resuming them would need per-entry skip
  logic and is a separate feature.
- Same-filesystem moves published by an atomic rename. They finish instantly and never
  create a partial.
- Browser uploads and downloads.
- Automatic resume on agent reconnect. The user always starts the resume.
- Any persistent transfer database on the server, and any in-memory resume state.

## Current behavior

| Area | Code | Behavior |
| --- | --- | --- |
| Staging names | `crates/redoor-core/src/staging.rs` (`StagingKind::sibling_of`) | `.{name}.redoor-upload-{random u64}` beside the destination. |
| Staging creation | `crates/redoor-agent/src/transfers/raw_upload.rs` (`create_staging_file`) | `create_new` so a colliding name or symlink is never followed or owned. |
| Abort | `raw_upload.rs` (`RawFileSink::abort`) | Always discards staging. |
| Publication | `raw_upload.rs` (`commit`), `transfers/upload.rs` (`PublicationGate`) | The router must authorize publication before the staging file is renamed into place. |
| Ranged source reads | `crates/redoor-protocol/src/wire/command.rs` (`Command::RawDownload`) | Already supports `range_start` / `range_end`. |
| Copy admission | `crates/redoor-server/src/http/services/transfer.rs` (`TransferService::start_copy`) | Reads source metadata and picks raw or tar transport. |
| Remote copy routing | `crates/redoor-server/src/actors/router/transfers/copy.rs` | Starts the destination upload, then the source download, joined by `CopyForwarder`. |
| Same-agent copy | `crates/redoor-agent/src/transfers/copy.rs` (`copy_file`, `stream_file_to_temp`) | Copies through a `StagingKind::LocalCopy` sibling with 1 MiB reads and discards it on any failure. |
| Same-agent move | `crates/redoor-agent/src/transfers/move.rs` (`move_entry`) | Atomic rename when on the same mount and the policy allows it; otherwise `copy_file` followed by the identity-checked source delete. |
| Disconnect | `crates/redoor-agent/src/runtime/sessions.rs` (`Sessions::invalidate`) | Cancels every running command, including local copies, through the same token a user cancel uses. |
| Source identity | `MoveSourceIdentity` in `wire/command.rs` | device, inode, size, mtime. Captured only for moves today. |
| Agent data dir | `crates/redoor-core/src/paths.rs` (`data_dir`) | `~/.local/share/<app-name>`. Already holds sessions, PID files, MCP state and logs. |
| Server secrets | `crates/redoor-core/src/config/bootstrap.rs`, `crates/redoor-server/src/http/auth.rs` | `agent_token` is shared with every agent, so it cannot be used to sign anything agents must not forge. |

## Design

### Partial file on the destination agent

A resumable copy stages into a partial file with a deterministic name so a later copy to the
same destination finds it:

```
{parent}/.{file name}.redoor-partial-{key}
```

- `key` is the first 16 hex characters of `blake3(absolute destination path)`.
- The file name part is truncated on a UTF-8 boundary so the full name stays under
  `NAME_MAX` (255 bytes). Without truncation, long destination names fail to stage, a
  problem the current random-suffix names also have.
- Add a `StagingKind::Partial` (tag `redoor-partial`) with a deterministic name builder
  instead of `sibling_of`, so every hidden-name scheme stays in `staging.rs`.
- The partial stays beside the destination so publication remains a same-filesystem
  rename through `place_temp_at_destination`.

Losing `create_new` removes the protections a random name gave. Opening the partial must
therefore:

- Use `O_NOFOLLOW`, then require a regular file owned by the agent's effective UID with
  `nlink == 1`. Otherwise refuse to resume and start fresh under a new partial (the stale
  entry is left alone, never deleted, because the agent does not own it).
- Check that the partial's device and inode match the manifest before trusting it.

Concurrent copies to the same destination must not share a partial. The destination agent
keeps an in-process lock table keyed by destination path. A second copy to a destination
whose partial is in use fails with a clear "destination has an active transfer" error. The
lock table only prevents concurrent use; it is not resume state and is empty after a
restart.

### Manifest in the agent data dir

Each partial has a manifest at:

```
{data_dir}/partials/{blake3(absolute destination path) hex}.json
```

`data_dir` is the existing `~/.local/share/<app-name>`, so `redoor` and `redoor-dev` agents
on the same machine keep separate manifests. Keeping manifests out of destination
directories means listing and cleanup find every partial by reading one small directory,
without scanning the filesystem. The file name stem is the manifest key used by the API.

The manifest holds everything needed to restart the transfer without the original request,
because nothing else remembers it.

Signed fields, chosen by the server when the transfer starts:

- `version` (so later releases can reject or migrate older formats)
- `source_agent_id`, `source_path`
- `source_identity`: device, inode, size, mtime seconds and nanoseconds
- `dest_agent_id`, `dest_path`
- `operation`: `copy` or `move`
- `on_existing`
- `created_at`
- `signature` over all of the above

Agent-local fields, written by the destination agent:

- `partial_path`, `partial_device`, `partial_inode`
- `updated_at`

The manifest is written atomically (write to a temporary file in the same directory, fsync,
rename) when the partial is created, and `updated_at` is refreshed when the transfer is
paused or interrupted. It is deleted after successful publication, on "Start over", on user
cancel, on Discard, and by cleanup.

The partial's current length is read from the file itself. It is not stored in the manifest
because the file length after a crash is the only value that reflects what was written.

### Manifest signatures

With resume driven by agent-written files, a compromised or untrusted destination agent
could otherwise write a manifest naming any source, for example
`server-agent:/home/user/.ssh/id_ed25519`. One Resume click would then copy that file to it,
or, with `operation: move`, delete it from the source. Today an agent only receives what a
user chooses to send it, and resume must not weaken that.

- The server keeps a 32-byte signing key in its data dir (`{data_dir}/partials-signing-key`,
  mode 0600), generated with `getrandom` on first use. It is never sent to agents. Unlike
  `agent_token`, no agent knows it.
- When a resumable transfer starts, the server computes
  `blake3::keyed_hash(key, canonical encoding of the signed fields)` and sends the signed
  fields and signature to the destination agent in the upload or local copy command. The
  agent stores them unchanged in the manifest.
- The server verifies the signature everywhere it reads a manifest: the resumable list, the
  per-destination probe and resume. A manifest that fails verification is ignored and
  logged at info level. It also checks that `dest_agent_id` is the agent that returned the
  manifest, so a valid manifest copied to another agent is rejected.
- If the key file is lost, every existing manifest stops verifying and those partials are
  no longer resumable. Cleanup still removes them by age.

### When the partial survives

The destination worker's abort path gets an explicit disposition instead of always
discarding:

| Event | Partial and manifest | Transfer outcome |
| --- | --- | --- |
| User cancels | Discarded (unchanged behavior) | `Canceled` |
| User pauses | Kept: flush, fsync, refresh manifest | Removed from transfer state (see Pause) |
| Transfer socket lost, either agent disconnects (same-agent copy: that agent disconnects) | Kept | `Errored`, message says the partial was kept |
| Source read error | Kept | `Errored`, message says the partial was kept |
| Destination write error (disk full, permissions) | Kept | `Errored`, message says the partial was kept |
| Prefix verification mismatch | Discarded | `Errored` |
| Publication succeeded | Partial renamed into place, manifest deleted | `Completed` |

If the destination agent itself crashes, nothing runs on its side. The partial and manifest
are already on disk from creation, so they show up in the resumable list once the agent
reconnects.

`TransferProgressState` gets no new states. Errored entries stay in the transfer history
like any other error, without a Resume button: resuming always goes through the resumable
list or a new copy, and always gets a new transfer id.

On the agent, a user cancel, a pause and a disconnect all arrive today as the same
cancelled `CancellationToken`, so the worker cannot tell whether to keep the partial. The
cancel registry therefore records a reason (`Canceled`, `Paused`, `Disconnected`) next to
the token. `Sessions::invalidate` sets `Disconnected`, and the server's cancel message
carries the other two. Resumable workers read the reason when they stop; every other
command keeps ignoring it.

### Pause

- New `POST /api/v1/transfers/{id}/pause`, accepted only for active resumable copies and
  moves before their publication boundary (the same boundary that makes `cancelable`
  false). A same-agent move that was published by an atomic rename is never pausable.
- Cross-agent: the router fails both streams with a "paused" disposition. The destination
  abort frame carries `keep_partial: true`, so the worker flushes, fsyncs and refreshes the
  manifest instead of discarding. The source download is canceled as it is today.
- Same-agent: the router sends `Cancel { id, reason: Paused }` to the agent. The local copy
  worker stops after its current write, flushes, fsyncs and refreshes the manifest.
- The endpoint responds only after the destination agent confirms that the partial and
  manifest are on disk. If the agent reports a failure instead, the response is an error
  and the transfer ends as `Errored`.
- On success, the router removes the transfer from its state entirely. No progress entry,
  flow or terminal record remains. The UI then refreshes the resumable list, where the
  paused transfer now appears.

### Resumable transfers list

Resumable transfers are discovered only from the agents:

- New agent command `ListPartials`. The agent reads `{data_dir}/partials/`, validates each
  partial file (see above), skips manifests whose destination is in its lock table (an
  active transfer), and returns each manifest with its key and current partial length. The
  directory holds one small file per partial, so this stays cheap even on slow disks.
- New `GET /api/v1/partials` (dedicated module `http/partials.rs`) sends `ListPartials` to
  every connected agent, verifies signatures, and returns the merged list. Offline agents
  are skipped: their partials cannot be resumed until they reconnect anyway.
- The list does not check whether each source still matches, since that would cost one
  metadata call per partial. A changed source is detected when resuming (see Detection).

Response item (dedicated struct with `#[ts(export)]`):

- `dest_agent_id`, `key`
- `source`, `dest` (endpoints), `operation`
- `partial_bytes`, `total_bytes` (the signed source size), `updated_at`

Actions:

- `POST /api/v1/partials/resume` with `{ dest_agent_id, key }`. The server reads the
  manifest from the agent itself (it does not trust endpoints sent by the client), verifies
  the signature, and starts the copy or move from the manifest's fields with
  `resume: "auto"`. Returns the new transfer id.
- `POST /api/v1/partials/discard` with `{ dest_agent_id, key }`. New agent command
  `DeletePartial { key }` removes the partial (only if its device and inode still match)
  and then the manifest.

UI: a "Resumable transfers" section next to the transfer list, fetched with TanStack Query.
Each row shows source, destination, operation and `partial_bytes / total_bytes`, with
Resume and Discard buttons. The query is invalidated after a pause, resume or discard, and
when an agent connects.

### Detection when initiating a copy

When the user initiates a copy or move, the UI first calls:

```
POST /api/v1/copy/preflight
```

with the source and destination endpoints. The server:

1. Reads the source metadata. Copies now capture the source identity too, not just moves.
2. Sends `ProbePartial { dest_path }` to the destination agent. The agent opens the manifest
   by key, validates the partial file, and returns the manifest plus the partial's current
   length, or nothing.
3. Verifies the signature, then compares the manifest's source agent, path and identity with
   the live source. Any difference means the partial is stale. Do not show this in the UI.
   Log at info level on the server with both identities, and return no candidate. The stale
   partial is replaced when the copy starts.

Response (dedicated struct with `#[ts(export)]`):

- `resume_candidate`: `null`, or `{ partial_bytes, total_bytes, updated_at }`.

The preflight never hashes. On slow spinning disks hashing can take minutes, so it would
block the dialog. Verification happens during the transfer instead.

The copy and move request structs gain `resume: "auto" | "restart"` (default `auto`):

- `auto` resumes when a valid candidate exists and starts fresh otherwise.
- `restart` discards any partial and starts from byte 0.

The MCP copy and move tools get the same field with the same default, so MCP clients resume
without having to ask.

The resume decision is independent of the existing destination conflict question. Both can
appear in the same dialog. A resumed copy still applies `on_existing` when it publishes.

Resuming from the list runs the same check as step 3 before starting, so a source changed
since the pause gives a fresh copy instead of a resume.

### Resume and blake3 verification

When a copy starts with a candidate of `N` bytes:

1. The destination agent opens the partial (validated as above) and truncates it to the
   last whole segment boundary at or below `N`, giving the resume offset `R`.
2. The destination hashes `[0..R)` in 64 MiB segments with blake3 and reports the segment
   digests. Hashing happens **before** appending: on a spinning disk, reading the prefix
   while writing the tail would make the disk head seek between both and slow both down.
   Reads use large sequential buffers (8 MiB).
3. The source agent hashes the same segments of the source file. It runs concurrently with
   the destination because they are different machines.
4. The router compares segment digests as they arrive. The first mismatching segment `k`
   sets the resume offset to `k × 64 MiB`. Comparison can stop at the first mismatch, which
   limits how much a slow disk has to read when the tail is torn.
5. The destination truncates to the final offset. The source streams with
   `RawDownload { range_start: offset }` and the destination appends.
6. Publication proceeds through the existing `PublicationGate`.

The source identity is checked again when the source opens the file for the ranged read.
If it changed since preflight, the transfer fails as `Errored` and the partial is
discarded, because the prefix may no longer belong to this source.

#### Same-agent variant

For a same-agent copy or copy-and-delete move, the whole verification runs inside the
agent's `LocalCopyFile` or `LocalMove` command, with no router round trips:

1. Open the partial (validated as above) and truncate it to the segment boundary `R`.
2. For each segment `k` below `R`: hash segment `k` of the source, then segment `k` of the
   partial, and compare. Stop at the first mismatch and truncate the partial there.
3. Seek the source to the resume offset and continue the normal copy loop, appending.

The two files are read alternately rather than in parallel. Source and partial are often on
the same physical disk (different partitions show different device ids, so the agent
cannot tell reliably), and alternating 64 MiB sequential reads keeps the disk head seeking
once per segment instead of constantly.

A same-agent move only creates a partial when it falls back to copy and delete. If a later
move to the same destination succeeds with an atomic rename instead, the agent removes the
leftover partial and its manifest after the rename publishes.

### Wire additions

All additive, defined on top of v2:

- `Command::ListPartials` → every valid, inactive manifest with its key and partial length.
- `Command::ProbePartial { dest_path }` → manifest and partial length, or none.
- `Command::DeletePartial { key }` → removes the partial and its manifest.
- `Command::HashSegments { path, segment_size, end }` → a stream of segment digests. Used on
  both agents so the hashing code is shared.
- `RawUpload` gains a resume mode (open the deterministic partial, truncate to a given
  offset, append) and carries the signed manifest fields and signature.
- `LocalCopyFile` and `LocalMove` gain the same resume mode (`auto` or `restart`) and signed
  manifest fields.
- The destination abort frame gains `keep_partial`.
- `ServerToAgent::Cancel` gains an optional `reason` (`Canceled` when absent, or `Paused`).
- A pause acknowledgement from the destination agent once the partial and manifest are
  durable.

### Progress reporting

- The transfer entry shows a `verifying` sub-state while segments are hashed, with
  `transferred_bytes` counting hashed bytes so slow disks show visible progress.
- After verification, `transferred_bytes` starts at the resume offset so the percentage
  reflects the whole file.
- Speed is computed only from bytes actually transferred in this attempt.

### Moves

A resumed move is a resumed copy followed by the existing identity-checked source delete.
The source identity from the signed manifest is compared again before deleting, as today.

### Cleanup

The destination agent sweeps `{data_dir}/partials/` on startup and then once a day. A
manifest is removed when:

- its partial file no longer exists, or
- `updated_at` is older than the configured age (default 7 days, agent config option).

For an expired manifest, the agent deletes the partial only if it is still a regular file
with the recorded device and inode. That check prevents deleting a file that was created at
the same path afterwards. The manifest is deleted afterwards. Partials are single files, so
this never involves recursive deletion.

The agent cannot verify signatures (it does not have the key), so cleanup relies only on age
and the device/inode check, never on signature validity.

A partial whose manifest was lost (data dir wiped, agent reinstalled) is not found by
listing or cleanup. Its deterministic name means the next copy to the same destination
replaces it, and the `.redoor-partial-` tag makes it recognizable to the user.

## Restarts

| Restart | Effect on resume |
| --- | --- |
| Server | None. The resumable list is rebuilt from the agents; the signing key is on disk. |
| Destination agent | None. Partials and manifests are on disk; they reappear once it reconnects. |
| Source agent | None. The source keeps no resume state. |
| Machine crash mid-write | Possibly a torn tail; segment verification resumes from the last matching 64 MiB boundary. |
| Server signing key lost | Existing partials are no longer resumable; cleanup removes them by age. |

## Implementation steps

1. **Core**: `StagingKind::Partial` with deterministic, length-safe names; manifest
   read/write/delete with atomic writes; partial open/validate helpers; canonical encoding
   of the signed fields. Unit tests for name truncation, symlink refusal, device/inode
   mismatch, atomic manifest writes and canonical encoding stability.
2. **Protocol**: the wire additions above; preflight, resumable list, resume and discard
   request and response structs; `resume` on the copy and move requests. Run
   `scripts/generate-ts-bindings`.
3. **Agent**: cancel reasons in the cancel registry; destination lock table; resumable
   `RawFileSink` (create or open the partial, write the manifest, keep or discard by
   disposition, acknowledge pause); resumable `copy_file` with the alternating segment
   verification, used by both `LocalCopyFile` and the copy-and-delete path of `LocalMove`;
   `ListPartials`, `ProbePartial`, `DeletePartial` and `HashSegments` handlers; cleanup
   sweep and its config option.
4. **Server**: signing key file and signature creation and verification; copy identity
   capture for copies; preflight endpoint; `http/partials.rs` with list, resume and discard;
   segment comparison and resume offset negotiation in the remote copy flow; pause endpoint
   that removes the transfer after the agent's acknowledgement; info logging for stale and
   unverifiable manifests.
5. **MCP**: `resume` field on the copy and move tools.
6. **UI**: preflight call in the copy and move dialog with a resume / start-over choice;
   Pause button on active resumable transfers (hidden for directory transfers);
   "Resumable transfers" section with Resume and Discard; `verifying` sub-state in the
   transfer list.

## Tests

Integration tests (`tests/`, vitest):

- Pause a copy: the pause response arrives, the transfer is gone from the progress API, and
  the resumable list shows it with the paused length. Resume from the list completes and
  the destination matches the source byte for byte.
- Pause, restart the server, and the resumable list still shows the transfer; resume
  completes.
- Pause, restart the destination agent, and the transfer reappears in the list after it
  reconnects.
- Interrupt a copy with toxiproxy (pattern from `raw-upload-toxiproxy.test.ts`); the entry
  ends as errored, the list shows the partial, and re-initiating the same copy resumes it.
- Modify the source after pausing; preflight reports no candidate, resuming from the list
  starts fresh, and the result matches the new source.
- Corrupt one byte in the middle of the partial; resume restarts from that segment and the
  result still matches the source.
- Tamper with a manifest's `source_path` on disk; the list omits it and resume by key fails.
- Copy a valid manifest into another agent's partials dir; that agent's list omits it.
- `resume: "restart"` discards the partial and copies from byte 0.
- Cancel discards the partial and its manifest.
- Discard from the list removes the partial and its manifest.
- A second copy to a destination with an active transfer fails with the active-transfer
  error, and the active transfer is not in the resumable list.
- A resumed move deletes the source only after publication.
- Cleanup removes an expired manifest and its partial, and leaves a partial whose inode no
  longer matches.
- MCP copy with the default `resume` resumes a paused partial.
- Same-agent copy: pause, the list shows it, resume completes with matching content.
- Same-agent copy interrupted by restarting the agent; re-initiating resumes.
- Same-agent copy with a corrupted partial segment resumes from that segment.
- Same-agent move with the `Merge` policy (forces copy and delete): pause, resume, and the
  source is deleted only after publication.
- Same-filesystem move to a destination with a leftover partial publishes by rename and
  removes the partial and its manifest.

Wait for log messages or poll the progress and partials APIs for state changes; never sleep.

Playwright:

- Primary workflow: start a cross-agent copy, pause it, see it in "Resumable transfers",
  click Resume, and see it complete.
- Re-initiating a paused copy from the file browser offers Resume in the copy dialog.
