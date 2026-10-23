# Review of commit 5dcdc0

Commit: `5dcdc057c07444da6f79c1c4d6a7357d91c3aa8a` — Add authenticated stateless MCP support.

Static review only; no tests were run. Line references refer to the reviewed commit.

## 1. [P1] Persisted router IDs can target another grant’s work after restart

Location: `src/server/mcp/tools.rs:674`

Cancellation uses the persisted `remote_id`, including for interrupted operations. Router IDs restart at 1, so an old operation can cancel an unrelated new command or transfer after a server restart. Transfer refresh can likewise read unrelated progress through a reused ID. The stable grant ownership check protects the old operation record but does not establish ownership of the new router operation.

Invalidate remote IDs on restart or bind them to a server instance, and prevent terminal operations from targeting live router work.

## 2. [P2] Concurrent retries can admit duplicate transfers

Location: `src/server/mcp/tools.rs:859`

The idempotency key lookup is separate from transfer admission and recording the result. Two concurrent requests with the same key can both see no entry and start separate transfers before either records its result.

Reserve the key atomically before admitting the transfer, and make concurrent retries share the admission result.

## 3. [P2] Idempotency fingerprints omit the operation type

Location: `src/server/mcp/tools.rs:854`

The fingerprint contains only arguments. Calling `copy_path`, then `move_path` with identical arguments and the same idempotency key, returns the copy’s result without moving the source or rejecting the conflicting request.

Include the tool name or operation kind in the fingerprint.

## 4. [P2] Transfer completion is lost unless clients poll soon enough

Location: `src/server/mcp/tools.rs:591`

Transfer operation state is refreshed only when polled and while the router still retains the corresponding progress entry. Router terminal history expires after one hour or when it exceeds 1,000 entries. If a client first polls after that entry has been removed, a completed transfer remains permanently `running` in the MCP registry. That state also prevents operation expiry.

Persist transfer completion independently of client polling, before router history can be pruned.

## 5. [P2] Prepared replacement uploads return unusable URLs

Location: `src/server/mcp/tools.rs:1177`

The prepare tool emits `?on_existing=replace`, but the upload handler deserializes `CopyExistingMode`, which accepts `override`, not `replace`. Following the returned URL for the documented replacement workflow therefore produces HTTP 400.

Validate and normalize the conflict mode before constructing the upload URL.
