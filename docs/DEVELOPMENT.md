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
- Implementation is complete; fork push and remote CI verification are pending.

## Extension points and operational details

- `src/server/task.rs`: durable task model, priority, expiration, deduplication
  and bounded wall-clock waits. It has no dependency on Boya types or semantics.
- `src/server/mod.rs`: single-owner state loop and the `Handler` interface.
  `configure`/`submit` validate client operations; `execute` returns new tasks or
  a recurrence delay. Future domains can be composed in an application handler.
- A single action runs at a time, serializing quota mutations. Read-only discovery
  is marked preemptible and low priority; check-in/out has higher priority than
  selection. Tasks have a 45-second timeout. Network latency can delay actions;
  this is a wall-clock scheduler, not a hard real-time guarantee.
- `src/boya_auto.rs` owns policy settings, one-minute discovery and course windows;
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
```

The live test refreshes authentication normally but never prints credentials.
Cross-platform CI runs tests and Clippy on macOS, Linux and Windows; results must
be checked before claiming those additional platforms are verified.

## Working rules

Do not use real selection/sign endpoints merely to smoke-test. Use unit tests and
isolated daemon instances for mutations, and real credentials only for read-only
API compatibility checks. Never include credentials or full auth responses here.
