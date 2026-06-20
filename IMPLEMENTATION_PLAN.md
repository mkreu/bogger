# Borg Reminder Implementation Plan

## Goal

Build a small Rust tool that lets you manually trigger Borg backups from the latest Snapper snapshots, while showing backup state in the `ashell` status bar.

Primary requirements:

- Display backup status in `ashell`
- Show time since last successful backup
- Show active states such as running, paused, and failed
- Surface stale backup state clearly
- Start a backup by clicking the bar item

## Proposed Architecture

Implement a single Rust binary named `borg-reminder` with several subcommands:

- `borg-reminder watch`
- `borg-reminder trigger`
- `borg-reminder backup`
- `borg-reminder status --json`
- `borg-reminder pause`
- `borg-reminder resume`

Use the binary in two roles:

- As a long-lived status producer for `ashell`
- As the actual backup runner invoked by a root-owned `systemd` service

## Architecture

`ashell` custom modules support:

- a clickable `command`
- a long-lived `listen_cmd` that streams compact JSON
- dynamic icon mapping
- an alert indicator

That maps well to a design where:

- `watch` emits status updates for the bar
- `trigger` starts a background backup job
- `backup` performs the real work outside the bar process

Running the backup through a root-owned `systemd` service gives:

- access to root-only Snapper snapshots
- asynchronous start from the bar
- clear separation between privileged backup logic and unprivileged UI logic
- logs via `journalctl -u`
- pause/resume control with signals

## File Layout

Runtime files:

- Config: `/etc/borg-reminder/config.toml`
- Persistent state: `/var/lib/borg-reminder/state.json`
- Lock: `/run/borg-reminder/lock`
- Systemd unit: `/etc/systemd/system/borg-reminder-backup.service`

Repository layout:

- `src/main.rs` for CLI bootstrap
- `src/config.rs` for config loading
- `src/state.rs` for persisted state
- `src/backup.rs` for snapper and borg execution
- `src/watch.rs` for `ashell` status streaming
- `src/systemd.rs` for service interaction

Keep the implementation minimal. Use `main.rs` plus a few focused modules.

## CLI Responsibilities

### `watch`

Long-lived process used by `ashell` `listen_cmd`.

Responsibilities:

- read the persistent state file written by the root backend
- calculate elapsed time since last success
- combine persistent backup history with the current systemd unit state
- classify state as idle, running, paused, stale, or error
- emit compact JSON only when the visible output changes

Example output:

```json
{"text":"2h","alt":"idle"}
{"text":"running","alt":"running"}
{"text":"paused","alt":"paused"}
{"text":"26h","alt":"stale"}
{"text":"failed","alt":"error"}
```

### `trigger`

Short-lived command used by the bar click action.

Responsibilities:

- check whether a backup is already active
- start the root `borg-reminder-backup.service` if not active
- exit quickly without performing the backup inline

Notes:

- `trigger` should start the root `borg-reminder-backup.service`, not run the backup inline.
- `trigger` uses `pkexec systemctl start borg-reminder-backup.service`.

### `backup`

Internal command executed by the root systemd service.

Responsibilities:

- acquire an exclusive app lock
- record `last_attempt_at` and `started_at`
- discover the latest Snapper snapshots for configured subvolumes
- invoke Borg against those snapshots
- persist success or error details
- update `last_run_result` on exit

### `status --json`

One-shot command for debugging and scripts.

Responsibilities:

- report the current derived state in machine-readable form

### `pause` and `resume`

Control commands for resource management.

Responsibilities:

- pause the active root systemd service with `SIGSTOP`
- resume it with `SIGCONT`
- update derived status accordingly

## State Model

Persist a small JSON state document with fields such as:

- `last_success_at`
- `last_attempt_at`
- `last_error`
- `last_run_result` (`success` or `error`)
- `active_job_id`
- `archives_created`
- `snapshots_used`

The root backend should write this file with root ownership and user-readable permissions so the unprivileged `watch` process can consume it safely.

Derived user-visible states:

- `idle`
- `running`
- `paused`
- `error`
- `stale`

Notes:

- derive `stale` from `last_success_at` plus configured thresholds
- derive `running` and `paused` from the systemd unit state
- use the persistent state file for backup history and the last outcome, not transient service truth

## Snapshot Strategy

Use Snapper as the source of truth for backup inputs.

Configured Snapper profiles will likely include:

- `root`
- `home`

For each configured profile:

1. query Snapper for the latest snapshot
2. resolve its filesystem path
3. select the Borg repository configured for that profile
4. pass that snapshot path to Borg

Create one Borg archive per Snapper profile per run.

Example archive names:

- `host-root-2026-06-20T13:00:00`
- `host-home-2026-06-20T13:00:00`

This avoids awkward path remapping and keeps archive provenance obvious.

## Backup Flow

Detailed flow for a user-triggered backup:

1. User clicks the `ashell` module.
2. `borg-reminder trigger` runs.
3. `trigger` starts the root `borg-reminder-backup.service` unless one is already active.
4. The service runs `borg-reminder backup`.
5. `backup` acquires a lock and records the start of the attempt.
6. `backup` resolves the latest Snapper snapshots for each configured profile.
7. `backup` runs `borg create` for each snapshot source.
8. On success, `last_success_at` and archive metadata are written.
9. On failure, error details are written.
10. `watch` notices the changed state and updates `ashell`.

## Backup Behavior

Initial behavior:

- `borg create` only
- backup from snapshot paths, never from live subvolumes
- read per-profile repository settings and credentials from the root-owned config file

Permissions note:

- the actual Borg run should execute as `root` so it can read root-only Snapper snapshots reliably
- user-space components should never need direct access to those snapshot trees
- the root service should not depend on the desktop user's shell environment, SSH agent, or session secrets

## Config Format

Suggested `config.toml`:

```toml
[general]
state_path = "/var/lib/borg-reminder/state.json"

[[profile]]
name = "root"
snapper_config = "root"
repo = "user@host:root-repo"
passphrase = "root-passphrase"
archive_prefix = "myhost-root"
extra_args = ["--stats", "--compression", "zstd"]

[[profile]]
name = "home"
snapper_config = "home"
repo = "user@host:home-repo"
passphrase = "home-passphrase"
archive_prefix = "myhost-home"
extra_args = ["--stats", "--compression", "zstd"]

[status]
warn_after_hours = 12
stale_after_hours = 24
poll_seconds = 15
```

Guidelines:

- install the config as `root:root` with mode `0600`
- store per-profile Borg credentials in this file and keep it root-readable only
- do not place this config under a user-writable path
- validate config values at startup

## Ashell Integration

Use a `CustomModule` of type `Button`.

Example target config:

```toml
[[CustomModule]]
name = "BorgReminder"
type = "Button"
icon = "󰣇"
command = "borg-reminder trigger"
listen_cmd = "borg-reminder watch"
alert = "stale|error"
```

Optional icon mapping can distinguish states:

- idle
- running
- paused
- stale
- error

Important note:

- `ashell` custom modules clearly support dynamic icons and an alert dot.
- Per-module text coloring may be limited, so the reliable signal for overdue backups is an alert dot plus distinct icon/text.

The `listen_cmd` side should read the persistent state file and query systemd for unit state; it should not call Snapper or Borg directly.

## Root Service And Permissions

Add a root-owned system service:

- `borg-reminder-backup.service`

The service should:

- execute `borg-reminder backup`
- be startable with `systemctl start borg-reminder-backup.service`
- expose logs in `journalctl -u borg-reminder-backup.service`
- update a persistent state file that is readable by the desktop user
- run on demand and exit when the backup finishes

Control commands:

- `pkexec systemctl start borg-reminder-backup.service`
- `pkexec systemctl kill -s STOP borg-reminder-backup.service`
- `pkexec systemctl kill -s CONT borg-reminder-backup.service`

This keeps the privileged backend simple while still giving the user-space bar process a stable interface through one persistent state file plus the systemd unit state.

## State File Contract

Use a single persistent state file as the main interface between the root backend and the user-space frontend.

- `/var/lib/borg-reminder/state.json`

The root backend should update this file at key moments:

- immediately when a backup starts
- when a backup succeeds
- when a backup fails

If `pause` and `resume` are implemented, those commands may also annotate the state file for debugging, but `watch` should still treat systemd as the authoritative source for transient active state.

Example shape:

```json
{
  "last_success_at": "2026-06-20T08:00:00Z",
  "last_attempt_at": "2026-06-20T12:00:00Z",
  "started_at": "2026-06-20T12:00:01Z",
  "last_run_result": "success",
  "last_error": null
}
```

`watch` should treat this file as the source of truth for backup history and combine it with the current systemd unit state for bar rendering.

This preserves `last_success_at` across reboots and avoids the need for a second `/run` status file.

## Error Handling

Handle these cases explicitly:

- Borg repo unavailable
- authentication failure
- Snapper config missing
- no snapshots found
- backup already running
- interrupted backup
- paused backup
- malformed config

User-facing behavior:

- surface `error` state in the bar
- preserve the last error message in state for inspection
- do not overwrite `last_success_at` on failed runs

## Logging

Write useful structured logs to stderr/stdout for systemd capture.

Log at minimum:

- backup start and end
- selected snapshots
- borg commands being executed without secrets
- success summary
- failure summary

Primary inspection path:

- `journalctl -u borg-reminder-backup.service`

## Concurrency and Locking

Prevent overlapping backups.

Requirements:

- use a lock file or file lock in the state directory
- refuse to start a second `backup` while one is active
- let `trigger` return success when a job is already running, but without starting another

This keeps the bar interaction safe even under repeated clicks.

## Implementation Phases

### Phase 1: Project Skeleton

- add CLI argument parsing
- add config loading
- add state model and persistence
- add basic status derivation

### Phase 2: Backup Runner

- discover latest Snapper snapshots
- run Borg against snapshot paths
- write start, success, and error state
- add concurrency lock

### Phase 3: Watch Mode

- implement long-lived JSON status output
- poll state and systemd service status
- compute human-readable elapsed time
- emit updates only on change

### Phase 4: Systemd Integration

- add root service unit example or install output
- wire `trigger`, `pause`, and `resume`
- wire the privileged start path
- verify running and paused detection through the systemd unit state

### Phase 5: Ashell Integration

- add example `CustomModule` config
- define icon mappings and alert behavior
- verify click-to-start workflow end to end

### Phase 6: Polish

- desktop notifications on success or failure
- richer `status --json` output
- optional prune/compact command
- installation documentation

## Testing Plan

### Unit Tests

- elapsed-time formatting
- stale-threshold classification
- config parsing and validation
- state serialization and deserialization
- snapshot selection parsing if command output is parsed

### Manual Tests

- first run with no previous backup
- successful root and home snapshot backup
- repeated click while backup is already running
- Borg network or authentication failure
- missing Snapper snapshot
- paused and resumed job
- stale threshold crossing after enough time passes
- verify the desktop user can read status but not modify it
- verify config and Borg credentials are readable only by root

## Future Work

- `borg prune` and `borg compact`
- rich notifications
- advanced archive naming policies
- multiple click actions or menus
