# bogger

Small Rust CLI for manually triggering Borg backups from Snapper snapshots and surfacing backup state to `ashell`.

## Commands

- `bogger watch`
- `bogger trigger`
- `bogger backup [--config PATH]`
- `bogger status`
- `bogger pause`
- `bogger resume`

Frontend commands do not read the backend config file. They use built-in defaults for the shared state file, service name, and status thresholds, with optional CLI overrides:

- `--state-path /var/lib/bogger/state.json`
- `--service-name bogger-backup.service`

Command-specific frontend overrides:

- `watch`: `--poll-seconds 15`, `--warn-after-hours 12`, `--stale-after-hours 24`

`status` prints the current derived state as a single word such as `idle`, `running`, `paused`, `error`, or `stale`.

## Config

Install a root-owned config at `/etc/bogger/config.toml` for `bogger backup`.

Example:

```toml
[general]
state_path = "/var/lib/bogger/state.json"
lock_path = "/run/bogger/lock"
mount_dir = "/run/bogger/mnt"

[[profile]]
name = "root"
snapper_config = "root"
repo = "user@host:root-repo"
passphrase = "root-passphrase"
archive_prefix = "myhost-root"
extra_args = ["--stats", "--compression", "zstd"]
exclude = ["var/cache", "var/tmp"]

[profile.prune]
keep_daily = 7
keep_weekly = 4
keep_monthly = 6

[[profile]]
name = "home"
snapper_config = "home"
repo = "user@host:home-repo"
passphrase = "home-passphrase"
archive_prefix = "myhost-home"
extra_args = ["--stats", "--compression", "zstd"]
exclude = ["*/.cache", "sh:**/node_modules"]

[profile.prune]
keep_within = "2d"
keep_daily = 7
keep_weekly = 4
keep_monthly = 12
```

Set `exclude` before the `[profile.prune]` table of the same profile. TOML adds every key after `[profile.prune]` to the prune table.

### How a profile is backed up

For each profile, `bogger backup`:

1. Selects the newest Snapper snapshot of the profile's subvolume.
2. Bind-mounts it at `<mount_dir>/<name>` (default `/run/bogger/mnt/<name>`). The path is the same on every run, so the borg files cache can skip unchanged files.
3. Runs `borg create` inside that mount with `.` as the root. Archived paths are therefore relative to the subvolume root, for example `user/.cache` for `/home/user/.cache`.
4. Unmounts the snapshot.
5. Marks the snapshot as backed up: runs `snapper -c <snapper_config> modify --userdata bogger=<archive> <number>`. The other userdata of the snapshot stays. `snapper list` shows the mark in the `User Data` column.
6. If `[profile.prune]` is set: runs `borg prune --list --glob-archives '<archive_prefix>-*'` with the keep rules, then `borg compact`.

All borg output goes to the journal.

Note: archives made before this layout stored the full snapshot path (`home/.snapshots/<n>/snapshot/...`). Restores and diffs against those older archives show the different path layout.

### Exclusion patterns

`exclude` is a list of borg exclude patterns. Each entry is given to `borg create` as `--exclude`. Patterns match paths relative to the subvolume root, without a leading `/`. Borg style prefixes (`fm:`, `sh:`, `re:`, `pp:`, `pf:`) work, see `borg help patterns`. Other flags such as `--exclude-caches` go into `extra_args`.

### Pruning

Pruning is optional. A profile without a `[profile.prune]` table is never pruned. Supported keys, mapped to the borg flag of the same name:

- `keep_within` (string, for example `"2d"`)
- `keep_last`, `keep_minutely`, `keep_hourly`, `keep_daily`, `keep_weekly`, `keep_monthly`, `keep_13weekly`, `keep_3monthly`, `keep_yearly` (integers, at least 1, or `-1` for no limit)

A rule that is not set keeps nothing: archives that no rule keeps are deleted. To keep for example all yearly archives forever, set `keep_yearly = -1`.

At least one rule must be set. Unknown keys are an error, so a misspelled rule cannot silently remove a keep rule.

`archive_prefix` must not contain `,` or `=`, because Snapper userdata uses them as separators.

Prune only touches archives named `<archive_prefix>-*`. Profiles on the same `repo` must not have overlapping prefixes (equal, or one prefix followed by `-` starting the other); bogger rejects such a config.

To preview a prune by hand:

```bash
borg prune --dry-run --list --glob-archives 'myhost-home-*' --keep-daily 7 user@host:home-repo
```

### Failures

- If a snapshot cannot be found or mounted, or `borg create` fails, the job stops and reports `error`.
- If unmount, the snapshot mark, prune or compact fails after the archive was created, bogger logs the error and goes on with the next profiles. At the end, the run reports `error`, but `last_success_at` is still updated, because the data is backed up.

Set permissions to `0600`.

The shared state file is intentionally minimal and only persists bar-relevant state:

- `last_success_at`
- `last_attempt_at`
- `started_at`
- `last_run_result`

Detailed failures and per-archive activity are logged to the journal instead of being stored in the state file.

## Arch Linux

Build and install the package from this repository with:

```bash
makepkg -si
```

That installs:

- `bogger` to `/usr/bin/bogger`
- `bogger-backup.service` to `/usr/lib/systemd/system/bogger-backup.service`
- `config.toml.example` to `/usr/share/doc/bogger/config.toml.example`
- `bogger.conf` to `/usr/lib/tmpfiles.d/bogger.conf`

Create the backend config before starting the service:

```bash
sudo install -Dm600 /usr/share/doc/bogger/config.toml.example /etc/bogger/config.toml
sudoedit /etc/bogger/config.toml
```

Create the state and lock directories immediately with:

```bash
sudo systemd-tmpfiles --create /usr/lib/tmpfiles.d/bogger.conf
```

Enable or start the packaged unit with:

```bash
sudo systemctl enable --now bogger-backup.service
```

Or run a backup on demand with:

```bash
sudo systemctl start bogger-backup.service
```

## systemd

The Arch package already installs a unit wired to `/usr/bin/bogger`. The example below is for manual installs.

Example unit:

```ini
[Unit]
Description=Bogger Backup
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/bogger backup
User=root
Group=root
PrivateMounts=yes

[Install]
WantedBy=multi-user.target
```

Control commands used by the CLI:

- `pkexec systemctl start bogger-backup.service`
- `pkexec systemctl kill -s STOP bogger-backup.service`
- `pkexec systemctl kill -s CONT bogger-backup.service`

## ashell

Example custom module:

```toml
[[CustomModule]]
name = "Bogger"
type = "Button"
icon = "󰣇"
command = "bogger trigger"
listen_cmd = "bogger watch"
alert = "stale|error"
```

`watch` emits compact JSON like:

```json
{"text":"2h","alt":"idle"}
{"text":"running","alt":"running"}
{"text":"paused","alt":"paused"}
{"text":"26h","alt":"stale"}
{"text":"failed","alt":"error"}
```

Install-ready examples are under `contrib/`.
