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
```

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
