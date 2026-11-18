# Background tasks development

## Goal and agreed behavior

Add a generic resident task server while preserving every existing CLI command.
The same executable acts as client or daemon. Initial automation is Boya:

- `boya auto select --arts 5 --labor 3` tops up the current semester's selected
  courses to each configured category target. Only courses with `sign_config`
  may be automatically selected. Already selected courses count toward targets.
- `boya auto sign` toggles continuous discovery of selected self-service courses,
  including future selections; schedules both check-in and check-out.
- `boya auto sign <ID>` submits check-in/out for one selected self-service course.
- `server status`, `server start`, `server off` control a local resident daemon.

## Architecture decisions

- Local loopback IPC, private endpoint metadata, a per-directory process lock,
  versioned JSON state and atomic replacement. No public network listener.
- Default auth/data location remains the executable's directory. `BUAA_DATA_DIR`
  allows isolated instances and testing without touching real credentials.
- Generic tasks contain a kind, JSON payload, absolute due/expiry timestamps,
  retry metadata and durable outcomes. Boya owns policies and payload handling.
- Recheck `SystemTime` at bounded intervals and before execution; wake on client
  requests. Recover interrupted tasks after restart; expire missed sign windows.
- A daemon has no interactive CAPTCHA solver. Authentication failure must remain
  visible in status/logs and allow an interactive `buaa login` followed by retry.

## Progress (2026-11-18)

- Created `codex/background-task-server` from clean upstream `main` (`7f0fb8a`).
- Inspected CLI and pinned `buaa_api` source, including course/sign data and time
  conversion. Existing authentication files are under `target/release/`.
- Implemented a generic `Handler` boundary, loopback IPC, private per-instance
  token, OS process lock, detached startup, persistent task state and shutdown.
- Added Boya quota policies, periodic selected-course discovery, individual
  check-in/out planning, remote enrollment confirmation and background CAPTCHA
  errors. Original interactive commands still use their existing handlers.
- Tests cover real loopback IPC with a mock handler, priority preemption,
  interrupted-task recovery, retry backoff, state-file validation, serial quota
  enforcement, remote enrollment confirmation and actual CLI process lifecycle.
  Loopback tests need network permission outside the desktop filesystem sandbox.
- Auth snapshots use short OS locks and atomic writes. Stale background sessions
  do not overwrite a newer login. CookieStore has different Serde vs file formats;
  always use its dedicated reader/writer for file compatibility (regression test).
- Read-only live API check passed: queried 100 courses on page 1, 2 selected
  self-service courses and 2 unexpired sign windows. No real selection, drop,
  check-in or check-out was submitted. Credentials were only refreshed normally.
- Default suite: 13 tests passed, plus one separately invoked read-only live test.
  This includes a detached process killed with SIGKILL and restarted with its
  tasks intact. Strict Clippy, formatting and optimized release build passed.
- README, changelog and a three-platform CI check workflow are added. The final
  release binary passed isolated start/status/configure/off smoke tests, and the
  original `boya selected` command passed using the existing account (read-only).
- The branch is pushed to `self_fork` (`Cheuring/buaa-cli`). A final policy
  reconciliation prevents an in-flight enrollment
  using older settings from adding sign tasks after automatic signing is disabled.
- Linux and macOS CI passed. Windows ran unusually long; std process source and
  [Windows handle inheritance documentation](https://learn.microsoft.com/en-us/windows/win32/sysinfo/handle-inheritance)
  show that redirected stdio does not exclude other inheritable handles. Windows
  startup now temporarily disables inheritance on the CLI's original stdio handles
  while spawning the daemon, preventing captured-output pipes from staying open.
  CLI tests have a 30-second watchdog; CI jobs have a 15-minute limit. The fix
  passed Windows CI, including real detached startup, captured-output completion,
  durable settings, restart, all unit tests and strict Clippy.
- Latest code validation: `9fd2de2`, [CI run 44138967488](https://github.com/Cheuring/buaa-cli/actions/runs/44138967488).
  Linux and Windows jobs passed. Latest macOS job is still queued awaiting a
  GitHub runner; local macOS tests, formatting, Clippy and release build passed,
  as did the earlier macOS CI job in run `44137371956`. The earlier two runs were
  canceled after being superseded; their old Windows processes lacked the fix.
  Recheck the latest macOS job before claiming all remote jobs are green.
- Implementation and documentation are complete, changes are committed and pushed,
  and no real-account automatic policy was enabled during testing. The final
  documentation-only commit uses `[skip ci]`; the code validation SHA is above.

## Follow-up: discovery interval and diagnostics

- Both Boya discovery tasks recur 30 minutes after successful completion; initial
  discovery is immediate. Upgrading preserves any next scan time already stored
  in state. The next successful scan schedules the new interval.
- `src/server/logging.rs` writes JSON Lines to `server/server.log` during execution
  (2 MiB per file, one retained previous file). Each task attempt carries its
  task key through scheduling, API calls and domain results. Process stderr,
  including startup failures, is captured separately in `server.stderr.log`.
- Backend request logs use an operation name, course ID or page when applicable,
  elapsed time and a typed error category. Do not log full API errors: their
  message or nested source may include authentication data or raw responses.
  Enrollment confirmation and check-in/out acceptance are separate events from
  the successful return of an API call.
- Local verification: formatting and strict Clippy passed. The offline test suite
  passed 15 tests, including log rotation, isolated daemon lifecycle and the
  log sequence for a failed attempt with a scheduled retry. Live tests are
  ignored by default.
- Real-account soak against runtime commit `9cd9b75` on 2026-11-18, 12:51–14:51
  (UTC+8): 7,200 seconds, four successful rounds for each discovery path, 36
  successful Boya API calls, and 25 successful local IPC health checks. All rounds
  scheduled their next run 1,800,000 ms after completion. Enrollment discovery
  took 19–27 s per scan; sign discovery found two active or upcoming windows
  each round. The isolated daemon shut down cleanly. The test did not write to
  the existing credential files or send enrollment, check-in or check-out
  requests, so live mutations remain outside this validation.
- Background signing targets the later of the remote window opening and five
  minutes before class starts (check-in) or ends (check-out), plus a stable
  random offset shorter than 30 seconds, limited to the time remaining in the
  window. The daemon persists a
  separate Boya seed and retains the window-based task keys. An existing
  installation refreshes automatic sign tasks on the first startup after
  upgrading; the execution handler also rechecks the rule on fresh course
  details. The legacy interactive `boya check <ID>` keeps its original timing.
  Seventeen offline tests and strict Clippy passed, including a legacy state
  refresh, rediscovery without a changed offset, and narrow-window scheduling.

## Extension points and operational details

- `src/server/task.rs`: durable task model, priority, expiration, deduplication
  and bounded wall-clock waits. It has no dependency on Boya types or semantics.
- `src/server/mod.rs`: single-owner state loop and the `Handler` interface.
  `prepare` initializes domain state on startup; `configure`/`submit` validate
  client operations; `execute` returns new tasks or
  a recurrence delay; `reconcile` validates results against the latest settings.
  Future domains can be composed in an application handler.
- A single action runs at a time, serializing quota mutations. Read-only discovery
  is marked preemptible and low priority; check-in/out has higher priority than
  selection. Tasks have a 45-second timeout. Network latency can delay actions;
  this is a wall-clock scheduler, not a hard real-time guarantee.
- `src/boya_auto.rs` owns policy settings, the shared 30-minute discovery interval
  and course windows;
  `src/boya_auto/backend.rs` adapts the pinned university API and allows mock tests.
- Enrollment handlers verify the selected list after mutation because the pinned
  upstream `select_course` does not parse its business-status response. Each retry
  reconciles remote selections before submitting again.
- Selection targets are persistent and update only supplied categories; `0`
  disables a category. They are applied to the server-reported current semester.
  Signing is independent; toggling off removes automatic pending tasks and keeps
  explicitly submitted course tasks. Windowed actions retry until expiry.
- State version is 1. Unsupported/corrupt state fails startup without resetting
  it. State is persisted before acknowledging configuration or dispatching work.
  Outcomes for active windows are retained for deduplication; old outcomes are
  compacted. Interrupted external effects have at-least-once retry semantics.
- The service survives terminal closure but does not install a boot service or
  keep a sleeping host awake. Use `server start` after reboot/crash. Tests stop
  their instances; no real-account automation policy is enabled during validation.

## Verification commands

```sh
cargo fmt --check
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
cargo build --offline --release

# Optional: university queries only; no enrollment or signing.
BUAA_LIVE_AUTH_DIR="$PWD/target/release" cargo test --offline \
  live_read_only_compatibility -- --ignored --nocapture

# Optional: two-hour real-account soak with the real scheduler, read-only
# Boya discovery, and a new directory for state and diagnostic logs.
BUAA_LIVE_AUTH_DIR="$PWD/target/release" \
BUAA_SOAK_DATA_DIR="$PWD/target/boya-soak" cargo test --offline \
  live_read_only_daemon_soak -- --ignored --nocapture
```

The short live test refreshes authentication normally but never prints
credentials. The soak keeps any refreshed session in memory, prevents mutations
at the backend boundary, and discards the planned course actions. Use a new
`BUAA_SOAK_DATA_DIR` for each run; the test shuts down its daemon after two hours
and leaves its log under that directory's `server/`.
Cross-platform CI runs tests and Clippy on macOS, Linux and Windows; results must
be checked before claiming those additional platforms are verified.

## Working rules

Do not use real selection/sign endpoints merely to smoke-test. Use unit tests and
isolated daemon instances for mutations, and real credentials only for read-only
API compatibility checks. Never include credentials or full auth responses here.
