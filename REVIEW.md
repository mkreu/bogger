# Review

1. High - The frontend commands should not depend on the backend config file.

   References: `src/main.rs:38-50`, `src/config.rs:64-70`, `README.md:16-48`

   This should be treated as two separate applications with a minimal shared state file contract: a privileged backend that reads `/etc/borg-reminder/config.toml` and performs backups, and an unprivileged frontend used by `ashell` that reads only the state file and controls the systemd service. In the current implementation, `main()` eagerly loads the full backend config before dispatching any subcommand, so `watch`, `trigger`, `pause`, `resume`, and `status --json` all depend on the backend config even though they should not need it. The frontend should instead use built-in defaults for paths, service name, and status thresholds, with optional CLI overrides for nonstandard setups.

2. Medium - The state file should contain only the minimal bar-relevant state, and detailed error text should stay in the journal.

   References: `src/state.rs:10-22`, `src/backup.rs:44-45`, `src/watch.rs:16-23`

   The bar only needs a small amount of persisted status such as `last_success_at`, `last_attempt_at`, `started_at`, and whether the last run succeeded or failed. The current state file also stores `archives_created`, `snapshots_used`, `active_job_id`, and raw `last_error`, which turns the state file into a debug/audit record. That detailed information should be logged to the journal instead, while the state file keeps only the minimal UI contract, for example a coarse error flag via `last_run_result`.

3. Medium - systemd query failures are silently downgraded to "no unit state", which can misreport live jobs as idle/stale and hide service misconfiguration.

   References: `src/systemd.rs:28-37`, `src/systemd.rs:47-49`, `src/watch.rs:36-39`, `src/watch.rs:63-75`, `src/main.rs:53-56`

   `query_unit_state_optional()` drops every `systemctl show` error and returns `None`. `watch` and `status --json` then fall back to the inactive-state path, so a bad service name, missing unit, or transient `systemctl` failure is rendered as normal idle/stale/error state from the persisted file instead of surfacing an operational problem. `trigger` also treats the same failure as "not running" and will attempt another `pkexec systemctl start`, which is noisy at best and misleading when the real issue is that service inspection is broken.
