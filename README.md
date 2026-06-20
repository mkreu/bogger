# borg-reminder

Small Rust CLI for manually triggering Borg backups from Snapper snapshots and surfacing backup state to `ashell`.

## Commands

- `borg-reminder watch`
- `borg-reminder trigger`
- `borg-reminder backup`
- `borg-reminder status --json`
- `borg-reminder pause`
- `borg-reminder resume`

## Config

Install a root-owned config at `/etc/borg-reminder/config.toml`.

Example:

```toml
[general]
state_path = "/var/lib/borg-reminder/state.json"
lock_path = "/run/borg-reminder/lock"
service_name = "borg-reminder-backup.service"

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

Set permissions to `0600`.

## systemd

Example unit:

```ini
[Unit]
Description=Borg Reminder Backup
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/borg-reminder backup
User=root
Group=root

[Install]
WantedBy=multi-user.target
```

Control commands used by the CLI:

- `pkexec systemctl start borg-reminder-backup.service`
- `pkexec systemctl kill -s STOP borg-reminder-backup.service`
- `pkexec systemctl kill -s CONT borg-reminder-backup.service`

## ashell

Example custom module:

```toml
[[CustomModule]]
name = "BorgReminder"
type = "Button"
icon = "󰣇"
command = "borg-reminder trigger"
listen_cmd = "borg-reminder watch"
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
