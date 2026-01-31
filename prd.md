# PM3 — PRD

A Rust-based process manager. Define processes in `pm3.toml`, manage them with simple commands.

---

## Config Format

`pm3.toml` in the project directory (like docker-compose.yml). Each `[section]` is a process:

```toml
[db]
command = "docker compose up postgres"
restart = "always"
group = "infra"

[web]
command = "node server.js"
cwd = "./frontend"
env = { PORT = "3000" }
env_file = ".env"
env_production = { NODE_ENV = "production", PORT = "8080" }
health_check = "http://localhost:3000/health"
kill_timeout = 5000
kill_signal = "SIGINT"
depends_on = ["db"]
group = "backend"
pre_start = "npm run build"
notify = "webhook://https://hooks.slack.com/..."
watch = "./src"
watch_ignore = ["node_modules", ".git", "logs"]
cron_restart = "0 3 * * *"
min_uptime = 5000
stop_exit_codes = [0]
log_date_format = "%Y-%m-%d %H:%M:%S"

[worker]
command = "python worker.py"
depends_on = ["db"]
max_restarts = 10
restart = "on-failure"
group = "backend"
```

Fields: `command` (required), `cwd`, `env`, `env_file`, `env_<name>`, `health_check`, `kill_timeout`, `kill_signal`, `max_restarts`, `max_memory`, `min_uptime`, `stop_exit_codes`, `watch`, `watch_ignore`, `depends_on`, `restart`, `group`, `pre_start`, `post_stop`, `notify`, `cron_restart`, `log_date_format`.

---

## Two Interfaces

1. **Interactive TUI** — run `pm3` with no arguments to open a full TUI where you can do everything visually
2. **CLI subcommands** — `pm3 start`, `pm3 stop`, `pm3 log`, etc. for scripting, CI, and quick one-offs

Both talk to the same background daemon.

---

## Daemon Architecture
- Single binary, dual-mode: CLI client by default, daemon with `--daemon` flag
- CLI communicates with daemon over a Unix domain socket (newline-delimited JSON)
- Daemon auto-starts when any CLI command is run (if not already running)
- PID file to track daemon process
- Daemon shuts down gracefully on SIGTERM/SIGINT (stops all children, saves state, cleans up socket + PID file)
- All daemon-side filesystem operations use `tokio::fs` (non-blocking); client-side code uses `std::fs` (blocking is acceptable pre-socket)

## Commands

| Command | Description |
|---|---|
| `pm3 start [name]` | Start all processes from pm3.toml (or just one by name) |
| `pm3 stop [name]` | Stop all (or one) |
| `pm3 restart [name]` | Stop + start |
| `pm3 list` / `pm3 view` | Table: name, PID, status, uptime, restarts |
| `pm3 log [name]` | Show recent log lines (stdout + stderr) |
| `pm3 kill` | Kill daemon and all managed processes |
| `pm3 reload [name]` | Zero-downtime reload (spawn new, then kill old) |
| `pm3 info <name>` | Detailed view of a single process |
| `pm3 init` | Interactive wizard to generate pm3.toml |
| `pm3 signal <name> <sig>` | Send an arbitrary signal (SIGHUP, SIGUSR1, etc.) |
| `pm3 save` | Snapshot current process list to disk |
| `pm3 resurrect` | Restore processes from last snapshot |
| `pm3 deploy <env>` | Deploy to remote servers over SSH |
| `pm3 startup` | Generate system service for boot auto-start |
| `pm3 unstartup` | Remove the generated service file |
| `pm3 flush [name]` | Clear log files |

## Health Checks
- Optional `health_check` field per process: `http://...` or `tcp://...`
- After spawning, status is `starting` (not `online`)
- Daemon polls health endpoint every 1s (up to 30s timeout)
- HTTP: GET the URL, 200 = healthy
- TCP: attempt connection, success = healthy
- Once healthy → status transitions to `online`
- If timeout → status becomes `unhealthy` (process keeps running, user is warned)
- Processes without `health_check` go straight to `online` after spawn
- Status values: `starting`, `online`, `unhealthy`, `stopped`, `errored`

## Process Dependencies
- `depends_on = ["db", "redis"]` config field
- Processes start in dependency order — a process won't launch until its dependencies are `online`
- On stop, dependents are stopped first (reverse order)
- Circular dependency detection at config parse time

## Zero-Downtime Reload
- `pm3 reload [name]` — spawn the new process before killing the old one
- No gap in service availability
- If the new process fails health check, the old one keeps running and the reload is aborted
- Falls back to a regular restart if reload isn't possible

## Restart Policy
- `restart` config field: `"on-failure"` (default), `"always"`, or `"never"`
- `on-failure` — restart only on non-zero exit
- `always` — restart regardless of exit code
- `never` — run once, don't restart
- `stop_exit_codes = [0]` — exit codes that should NOT trigger a restart (even under `on-failure`)

## Auto-Restart
- Automatically restart processes based on restart policy
- Configurable max restart count (default 15) to prevent infinite loops
- Exponential backoff between restarts
- Track restart count per process
- `min_uptime = 5000` — if a process crashes within this window (ms), it counts toward `max_restarts`. Restarts after stable uptime reset the counter

## Cron-Based Restart
- `cron_restart = "0 3 * * *"` config field
- Schedule periodic restarts using cron syntax
- Useful for clearing memory leaks, refreshing state, etc.

## Log Management
- Capture each process's stdout and stderr to separate log files (`<name>-out.log`, `<name>-err.log`)
- `pm3 log [name]` — show recent log lines (default last 15 lines)
  - `--lines <n>` — number of lines to show
  - `--follow` / `-f` — stream logs in real-time
  - No name = interleave logs from all processes, prefixed with process name
- `pm3 flush [name]` — clear log files
- Log rotation: rotate when file exceeds 10MB, keep last 3 rotated files
- `log_date_format = "%Y-%m-%d %H:%M:%S"` — prefix log lines with timestamps

## Env File Support
- `env_file = ".env"` config field (or an array: `env_file = [".env", ".env.local"]`)
- Loaded before inline `env` values, so inline takes precedence
- Standard `KEY=VALUE` format, `#` comments, blank lines ignored

## Per-Environment Config
- `env_production = { NODE_ENV = "production", PORT = "8080" }` config field
- `env_staging = { NODE_ENV = "staging" }` config field
- Switch environments: `pm3 start --env production`
- Base `env` values are always loaded, environment-specific values override them

## Process Groups
- `group = "backend"` config field to tag processes
- `pm3 start backend` — start all processes in the group
- `pm3 stop backend`, `pm3 restart backend` — operate on a group
- Groups shown in `pm3 list` output
- A process can belong to one group

## Lifecycle Hooks
- `pre_start = "npm run build"` — run a command before the process starts
- `post_stop = "cleanup.sh"` — run a command after the process stops
- Hooks run synchronously; if `pre_start` fails (non-zero exit), the process won't start
- Hook stdout/stderr captured in the process's log files

## Crash Notifications
- `notify` config field per process
- `notify = "webhook://https://..."` — POST JSON payload to the URL on crash/unhealthy
- `notify = "telegram://<bot_token>@<chat_id>"` — send a message via Telegram Bot API
- `notify = "desktop"` — OS-level desktop notification (macOS/Linux)
- Payload includes: process name, exit code, restart count, timestamp

## Signals
- `pm3 signal <name> <signal>` — send an arbitrary signal to a process
- Useful for config reloads (SIGHUP), debug toggling (SIGUSR1), etc.
- Signal names: SIGHUP, SIGUSR1, SIGUSR2, etc.

## Init
- `pm3 init` — interactive wizard that asks questions to generate pm3.toml
- Prompts for: process name, command, working directory, env vars, health check, restart policy, dependencies, group
- Asks "Add another process?" to define multiple processes in one session
- Scans the current directory for hints (Procfile, package.json scripts, docker-compose.yml) and suggests defaults
- Writes the final pm3.toml to the current directory

## Graceful Shutdown
- `kill_timeout` config field (default 5000ms)
- Stop sequence: send kill signal → wait timeout → SIGKILL
- `kill_signal = "SIGINT"` config field to customize the shutdown signal (default SIGTERM)
- Per-process configurable timeout and signal

## Max Memory Restart
- `max_memory` config field (e.g., `"200M"`, `"1G"`)
- Daemon monitors memory usage and auto-restarts process if limit exceeded

## Watch Mode
- `watch = true` or `watch = "./src"` config field
- Auto-restart the process when files change in the watched directory
- `watch_ignore = ["node_modules", ".git", "logs"]` — exclude directories/files from triggering restarts
- Debounce file change events (500ms)

## Process Info
- `pm3 info <name>` — detailed view of a single process
- Shows: PID, status, command, args, cwd, env vars, uptime, restarts, exit code, log file paths, CPU%, memory, health check status, group, dependencies

## Interactive TUI
- `pm3` with no arguments (or `pm3 tui`) opens a full interactive TUI
- The TUI is the primary interface — all features accessible from one screen
- Panels / views:
  - **Process list**: live status table (name, PID, status, uptime, restarts, CPU%, memory)
  - **Logs**: view stdout/stderr for the selected process, auto-scrolling
  - **Config editor**: edit pm3.toml visually — add, remove, or modify process definitions, save and apply
  - **Actions**: start, stop, restart selected process or all processes
- Keyboard-driven: arrow keys to navigate, enter to select, `q` to quit
- Background stats collection in daemon (poll CPU/memory every 2s)

## State Persistence
- Daemon persists process state to a JSON dump file
- On daemon restart, restore process list and check which PIDs are still alive
- Processes that died while daemon was down get auto-restarted

## Save & Resurrect
- `pm3 save` — snapshot the current running process list to disk
- `pm3 resurrect` — restore and start all processes from the last snapshot
- Useful when the daemon restarts — avoids re-running `pm3 start` in each project directory

## Deployment
- `pm3 deploy <env>` — deploy to remote servers over SSH
- Configured in pm3.toml under a `[deploy]` section:
  ```toml
  [deploy.production]
  host = "server.example.com"
  user = "deploy"
  path = "/var/www/app"
  repo = "git@github.com:user/repo.git"
  ref = "origin/main"
  pre_deploy = "git pull"
  post_deploy = "pm3 start"
  ```
- Commands: `pm3 deploy production setup`, `pm3 deploy production`, `pm3 deploy production revert`
- Lifecycle hooks: `pre_deploy`, `post_deploy`

## Startup Script Generation
- `pm3 startup` — generate a system service file for boot auto-start
  - macOS: launchd plist
  - Linux: systemd unit file
- `pm3 unstartup` — remove the generated service file

## Directory Layout
```
~/.local/share/pm3/    (Linux, via XDG)
~/Library/Application Support/pm3/    (macOS)
  pm3.pid
  pm3.sock
  dump.json
  logs/
    <name>-out.log
    <name>-err.log
```

---

## Testing Policy

Every step must be thoroughly tested before moving to the next. No exceptions.

- **Unit tests** for every module — pure logic tested in isolation (config parsing, protocol serialization, path resolution, cron parsing, env file parsing, dependency graph, etc.)
- **Integration tests** for every command — spin up a real daemon, send real requests over the socket, verify real process behavior
- **End-to-end tests** for every user-facing workflow — run the `pm3` binary as a subprocess, check stdout, verify processes actually start/stop
- Tests live in `tests/` (integration/e2e) and inline `#[cfg(test)]` modules (unit tests)
- Use `assert_cmd` + `predicates` for CLI testing, `tempfile` for isolated test directories
- Each phase must have all tests passing before the next phase begins

---

## Implementation Order

### Phase 1 — Core loop
1. ~~Config parsing — parse `pm3.toml` into `HashMap<String, ProcessConfig>`~~ **DONE**
   - Unit: valid TOML parses correctly, missing `command` errors, unknown fields error, empty file errors
   - Unit: all optional fields default correctly
   - Unit: multiple process sections parse into correct map keys

2. ~~Paths module — resolve data directory, socket path, PID file, log dir~~ **DONE**
   - Unit: Linux returns `~/.local/share/pm3/`, macOS returns `~/Library/Application Support/pm3/`
   - Unit: socket path, PID file path, log dir path all resolve under data dir
   - Unit: log file paths include process name (`<name>-out.log`, `<name>-err.log`)

3. ~~IPC protocol — define `Request`/`Response` enums with serde JSON~~ **DONE**
   - Unit: every `Request` variant serializes and deserializes roundtrip
   - Unit: every `Response` variant serializes and deserializes roundtrip
   - Unit: malformed JSON returns a clear error

4. ~~CLI parsing — clap subcommands: `start`, `stop`, `restart`, `list`, `kill`~~ **DONE**
   - Unit: each subcommand parses correctly from arg strings
   - Unit: `start` with no name → `names: None`, `start web` → `names: Some(["web"])`
   - Unit: unknown subcommand errors

5. ~~Daemon — daemonize, bind Unix socket, accept connections, dispatch requests~~ **DONE**
   - Integration: daemon starts, PID file is created, socket file exists
   - Integration: client connects, sends a request, gets a response
   - Integration: daemon handles multiple sequential connections
   - Integration: daemon won't start if another instance is already running (PID file check)
   - Refinement: daemon uses `tokio::fs` for all filesystem ops (create_dir_all, remove_file); PID module exposes async functions for daemon and `is_daemon_running_sync` for the client

6. ~~Process spawning — spawn child process with `command` and `cwd`, track PID~~ **DONE**
   - Integration: spawn `sleep 999`, verify PID is tracked and process is running
   - Integration: spawn with `cwd`, verify child's working directory
   - Unit: command string splits into program + args correctly
   - Refinement: `spawn_process` is async; uses `tokio::fs::create_dir_all` and `tokio::fs::File::create(...).into_std().await` for log file creation

7. ~~Log capture — pipe child stdout/stderr to log files~~ ✅
   - Integration: spawn `echo hello`, verify `<name>-out.log` contains "hello"
   - Integration: spawn a process that writes to stderr, verify `<name>-err.log`
   - Integration: log directory is created if it doesn't exist

8. ~~Start command — client reads pm3.toml, sends to daemon, daemon spawns processes~~ ✅
   - E2E: create pm3.toml with one process, run `pm3 start`, verify process is running
   - E2E: create pm3.toml with two processes, run `pm3 start`, both are running
   - E2E: `pm3 start web` starts only the named process
   - E2E: `pm3 start` with no pm3.toml prints an error
   - E2E: `pm3 start nonexistent` prints an error

9. ~~List command — daemon returns process table, client prints it~~ ✅
   - E2E: start processes, run `pm3 list`, output contains process names, PIDs, status
   - E2E: no processes running → `pm3 list` shows empty table or message
   - Integration: verify `ProcessInfo` struct has correct fields (name, PID, status, uptime, restarts)

10. ~~Graceful stop — SIGTERM → wait `kill_timeout` → SIGKILL, custom `kill_signal`~~ ✅
    - Integration: stop a process that handles SIGTERM, verify it exits cleanly
    - Integration: stop a process that ignores SIGTERM, verify SIGKILL after timeout
    - Integration: custom `kill_signal = "SIGINT"`, verify SIGINT is sent first
    - Unit: default `kill_timeout` is 5000ms

11. ~~Stop command — stop processes by name or all~~ ✅
    - E2E: `pm3 stop web` stops one process, others keep running
    - E2E: `pm3 stop` stops all processes
    - E2E: `pm3 stop nonexistent` prints an error

12. ~~Restart command — stop + start~~ ✅
    - E2E: `pm3 restart web` — process gets a new PID
    - E2E: `pm3 restart` — all processes get new PIDs
    - Integration: restart preserves the process config

13. ~~Kill command — stop all processes, shut down daemon, clean up~~ ✅
    - E2E: `pm3 kill` — all processes stopped, daemon exits, socket + PID file removed
    - E2E: subsequent `pm3 list` auto-starts a fresh daemon

### Phase 2 — Logs and restart
14. ~~Log command — read log files, show last N lines, `--follow`~~ ✅
    - E2E: start a process that prints output, `pm3 log web` shows lines
    - E2E: `pm3 log --lines 5` shows exactly 5 lines
    - E2E: `pm3 log` with no name shows interleaved logs from all processes
    - Integration: `--follow` streams new lines as they appear (test with timeout)

15. ~~Flush command — clear log files~~ ✅
    - E2E: `pm3 flush web` — log files for web are emptied
    - E2E: `pm3 flush` — all log files emptied
    - Integration: verify file exists but is empty after flush

16. ~~Log timestamps — `log_date_format` prefix~~ ✅
    - Integration: set `log_date_format`, spawn process, verify log lines start with timestamp
    - Unit: timestamp formatting with various format strings
    - Integration: no `log_date_format` → no timestamp prefix

17. ~~Log rotation — rotate at 10MB, keep 3 old files~~ ✅
    - Integration: write >10MB to a log, verify rotation creates `<name>-out.log.1`
    - Integration: verify only 3 rotated files are kept, oldest is deleted
    - Unit: rotation threshold check logic

18. ~~Restart policy — `restart` field, `stop_exit_codes`~~ ✅
    - Unit: parse `"on-failure"`, `"always"`, `"never"` from config
    - Unit: default is `"on-failure"`
    - Unit: `stop_exit_codes` correctly marks exit codes as non-restartable
    - Integration: `restart = "never"`, process exits → not restarted
    - Integration: `restart = "always"`, process exits with 0 → restarted
    - Integration: `restart = "on-failure"`, exit 0 → not restarted, exit 1 → restarted
    - Integration: `stop_exit_codes = [42]`, exit 42 → not restarted even under `on-failure`

19. ~~Auto-restart — detect child exit, restart based on policy, track count~~ ✅
    - Integration: process crashes, daemon restarts it, restart count increments
    - Integration: restart count reaches `max_restarts`, process is NOT restarted, status → `errored`
    - Integration: `pm3 list` shows correct restart count

20. ~~Exponential backoff — increasing delay between restarts~~ ✅
    - Unit: backoff sequence is correct (e.g., 100ms, 200ms, 400ms, ...)
    - Integration: rapid crashes → delays increase between restarts
    - Unit: backoff caps at a maximum delay

21. ~~min_uptime — reset restart counter after stable uptime~~ ✅
    - Integration: process runs longer than `min_uptime`, crashes → restart count resets to 0
    - Integration: process crashes within `min_uptime` → restart count increments
    - Unit: uptime comparison logic

### Phase 3 — Environment and config
22. Environment variables — pass `env` from config to child process
    - Integration: set `env = { FOO = "bar" }`, spawn process that prints `$FOO`, verify output
    - Integration: multiple env vars passed correctly
    - Integration: env vars don't leak between processes

23. Env file support — load `.env` files
    - Unit: parse `.env` file: `KEY=VALUE`, comments, blank lines, quoted values
    - Unit: `env_file` as string and as array both work
    - Integration: env file values are available in the child process
    - Integration: inline `env` overrides `env_file` values
    - Integration: missing env file prints an error

24. Per-environment config — `env_production` sections, `--env` flag
    - Unit: `env_production` parsed from config
    - Integration: `pm3 start --env production` merges base `env` + `env_production`
    - Integration: production values override base values
    - Integration: `--env` with unknown environment name errors

25. Process info command — `pm3 info <name>`
    - E2E: `pm3 info web` prints PID, status, command, cwd, env, uptime, restarts, log paths
    - E2E: `pm3 info nonexistent` prints an error
    - Integration: all fields populated correctly

### Phase 4 — Health checks and dependencies
26. Health checks — HTTP GET / TCP connect polling, status transitions
    - Integration: start process with HTTP health check, status goes `starting` → `online`
    - Integration: health check URL returns non-200, status stays `starting` then → `unhealthy` after timeout
    - Integration: TCP health check connects successfully → `online`
    - Integration: no health check → status goes straight to `online`
    - Unit: parse `http://...` and `tcp://...` health check URLs
    - Integration: health check timeout (30s) triggers `unhealthy`

27. Process dependencies — `depends_on`, topological sort, circular detection
    - Unit: topological sort produces correct start order
    - Unit: circular dependency detected and returns error
    - Unit: missing dependency name returns error
    - Integration: `web` depends on `db` → `db` starts first, `web` waits until `db` is `online`
    - Integration: stop with dependencies → dependents stopped first (reverse order)

28. Process groups — `group` field, resolve group names in commands
    - Integration: `pm3 start backend` starts all processes with `group = "backend"`
    - Integration: `pm3 stop backend` stops the group
    - E2E: `pm3 list` shows group column
    - Unit: group name resolution — process name takes priority over group name if conflict

### Phase 5 — Lifecycle and signals
29. Signal command — `pm3 signal <name> <sig>`
    - Integration: send SIGUSR1 to a process, verify it received the signal
    - E2E: `pm3 signal web SIGHUP` succeeds
    - E2E: `pm3 signal nonexistent SIGHUP` errors
    - Unit: signal name parsing (SIGHUP, SIGUSR1, SIGUSR2, etc.)

30. Lifecycle hooks — `pre_start`, `post_stop`
    - Integration: `pre_start = "echo before"` runs before process starts, output in logs
    - Integration: `pre_start` fails (exit 1) → process does NOT start
    - Integration: `post_stop = "echo after"` runs after process stops
    - Integration: on restart, sequence is `post_stop` → `pre_start` → start

31. Zero-downtime reload — `pm3 reload`, spawn new before killing old
    - Integration: reload spawns new process, waits for health check, then kills old
    - Integration: new process fails health check → old keeps running, reload aborted
    - Integration: process without health check → reload falls back to restart
    - E2E: `pm3 reload web` — PID changes, no downtime gap

### Phase 6 — Monitoring and resource limits
32. Max memory restart — poll memory, restart if over `max_memory`
    - Unit: parse `"200M"`, `"1G"` into bytes
    - Integration: process exceeds memory limit → daemon restarts it
    - Integration: memory check interval is reasonable (not spinning CPU)

33. Watch mode — file change detection, debounce, auto-restart
    - Integration: `watch = "./src"`, modify a file in `./src` → process restarts
    - Integration: debounce — rapid file changes trigger only one restart
    - Integration: `watch = true` watches the process's `cwd`

34. Watch ignore — `watch_ignore` exclusion patterns
    - Integration: changes in ignored directories do NOT trigger restart
    - Unit: glob pattern matching for ignore list

35. Cron-based restart — parse cron expression, schedule restarts
    - Unit: cron expression parsing (`"0 3 * * *"`, `"*/5 * * * *"`)
    - Unit: next run time calculation
    - Integration: cron triggers a restart at the scheduled time (use short interval for test)

### Phase 7 — Persistence
36. State persistence — daemon writes state to `dump.json`, restores on startup
    - Integration: start processes, verify `dump.json` is written
    - Integration: kill daemon, restart it, verify process list is restored from dump
    - Integration: restored processes — check which PIDs are still alive, mark dead ones as `errored`
    - Unit: dump file serialization/deserialization roundtrip

37. Save & resurrect — `pm3 save` / `pm3 resurrect`
    - E2E: `pm3 save` creates snapshot file
    - E2E: `pm3 kill`, then `pm3 resurrect` restarts all previously saved processes
    - Integration: resurrect from a different directory works (snapshot stores absolute paths)

### Phase 8 — Notifications
38. Crash notifications (webhook) — POST JSON on crash/unhealthy
    - Integration: start a mock HTTP server, configure webhook notify, crash process → mock receives POST
    - Unit: notification payload contains process name, exit code, restart count, timestamp
    - Integration: webhook failure doesn't block restart

39. Crash notifications (Telegram) — send via Telegram Bot API
    - Unit: Telegram URL construction from `telegram://<token>@<chat_id>`
    - Integration: mock Telegram API endpoint, verify correct request format
    - Integration: Telegram failure doesn't block restart

40. Crash notifications (desktop) — OS-level notification
    - Unit: notification message formatting
    - Integration: verify notification library is called on crash (mock/spy)

### Phase 9 — TUI
41. Interactive TUI — process list panel with live status
    - Integration: TUI renders process table with correct data from daemon
    - Integration: table updates when process status changes

42. TUI log viewer — view logs for selected process
    - Integration: selecting a process shows its log output
    - Integration: logs auto-scroll as new lines appear

43. TUI actions — start/stop/restart from TUI
    - Integration: pressing action keys sends correct request to daemon
    - Integration: process status updates in table after action

44. TUI config editor — edit pm3.toml visually
    - Integration: editor loads current pm3.toml content
    - Integration: saving writes valid TOML back to file
    - Integration: applying config changes restarts affected processes

### Phase 10 — Init, deploy, startup
45. Init wizard — `pm3 init`, interactive prompts, scan for existing configs
    - E2E: run `pm3 init` in a directory with package.json, verify suggested defaults
    - E2E: generated pm3.toml is valid and parseable
    - Integration: scanning Procfile, docker-compose.yml produces correct suggestions
    - E2E: `pm3 init` in a directory with existing pm3.toml warns before overwriting

46. Startup script generation — `pm3 startup` / `pm3 unstartup`
    - Integration: on macOS, generates a valid launchd plist
    - Integration: on Linux, generates a valid systemd unit file
    - E2E: `pm3 unstartup` removes the generated file
    - Unit: generated file content is correct (paths, user, etc.)

47. Deployment — `pm3 deploy`, SSH-based with hooks and rollback
    - Unit: deploy config parsing from `[deploy.production]`
    - Integration: `pm3 deploy production setup` runs `pre_setup` hook
    - Integration: `pm3 deploy production` runs git pull + `post_deploy` hook
    - Integration: `pm3 deploy production revert` rolls back to previous deployment
    - E2E: deploy to a local SSH target (localhost) end-to-end
