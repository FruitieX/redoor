# SSH relays

A relay starts a Redoor agent on an SSH host and routes its WebSocket connection through the machine running the relay. Configure any number of relays; Redoor starts only the relay named by a command.

## Configuration

Relays are `[[relays]]` entries in the shared `config.toml`. Each entry requires a stable `id`, SSH `target`, and Redoor `server`. The shared top-level `agent_token` authenticates the remote agent.

```toml
agent_token = "long-private-secret"

[[relays]]
id = "production"
target = "user@production.example.com"
server = "https://redoor.example.com"
name = "production-agent"
agent_app_name = "redoor-production-agent"

[[relays]]
id = "staging"
target = "staging.example.com"
server = "https://staging-redoor.example.com"
ssh_port = 2222
```

An ID may contain ASCII letters, numbers, `.`, `_`, and `-`. It identifies local lifecycle state and remains independent from `name`, which names the remote device in the UI and remote CLI. Relay IDs must be unique.

`relay start` writes an `agent_id = "dev_..."` line into the entry the first time it runs. It is the remote device's stable id, which access token scopes refer to, so do not change or copy it. If the config file is read-only, the command fails and prints the line to add.

`agent_app_name` isolates the remote agent's PID and data files. When omitted, it defaults to `<local-app-name>-relay-<relay-id>`, allowing multiple relays to run concurrently on the same SSH host. Explicit values use the same validation as `--app-name`.

## Lifecycle

Start one relay in the foreground:

```bash
redoor agent relay start production
```

Detach it:

```bash
redoor agent relay start production --daemon
```

Foreground and daemon relays both supervise their SSH session. Failed host preparation, remote-port conflicts, and later SSH exits are retried with bounded exponential backoff until the relay is stopped.

Manage that relay without starting any other configured relay:

```bash
redoor agent relay ls
redoor agent relay status production
redoor agent relay logs production
redoor agent relay logs production -n 100
redoor agent relay stop production
```

Use a non-default config when listing or starting relays, or when resolving logs for a stopped relay:

```bash
redoor agent relay start production --config /path/to/config.toml --daemon
redoor agent relay logs production --config /path/to/config.toml
redoor agent relay ls --config /path/to/config.toml
```

`ls` prints the configured relay IDs one per line in declaration order. `status` and `stop` use only the relay ID and its runtime file. They continue to work if the relay entry is changed or removed while its process is running. `logs` also prefers the running relay's recorded log path.

## Services

Install a relay as a systemd service on Linux or a LaunchAgent on macOS so it starts at boot or login and is restarted when it fails. Each relay gets its own service, named `redoor-relay-<id>` (the `redoor` prefix follows `--app-name`):

```bash
redoor agent relay systemd install production --start
redoor agent relay launchd install production --start
```

The service runs `redoor agent relay start <id>` in the foreground with the default config path pinned. Installation fails unless that config exists and contains a `[[relays]]` entry with the given ID.

The relay ID follows the action, and every action of the agent and server services is available:

```bash
redoor agent relay systemd start production
redoor agent relay systemd stop production
redoor agent relay systemd restart production
redoor agent relay systemd status production
redoor agent relay systemd enable production
redoor agent relay systemd disable production --now
redoor agent relay systemd uninstall production
```

Replace `systemd` with `launchd` on macOS. Restart the service after changing the relay's entry. As with the agent, running `systemd` as root installs a system unit that runs the relay as root.

systemd services do not inherit a login session's environment, so an `ssh-agent` started for a terminal session (`SSH_AUTH_SOCK`) is not available to them. Use an SSH key that `ssh` can load without prompting, for example through `~/.ssh/config`.

## Runtime files

Each relay holds an advisory lock on its own JSON PID file for its process lifetime:

```text
~/.local/share/redoor/relays/production.pid
```

The `redoor` path segment follows `--app-name` / `REDOOR_APP_NAME`. File contents include the PID, relay ID, start time, target, server, effective agent name, remote agent app name, and log path. They never include the agent token.

The file lock is authoritative for liveness. An unlocked stale file is removed by `status` or `stop` and can be replaced by the next start.

Default relay logs are isolated by ID:

```text
~/.local/share/redoor/relays/production.log
```

Root uses `/var/log/redoor/relays/production.log` by default.

## Startup options

Every relay setting lives in its `[[relays]]` entry. `--daemon` is a `start` option. `REDOOR_AGENT_TOKEN` may override the top-level token at startup; relay-specific settings do not have environment overrides.
