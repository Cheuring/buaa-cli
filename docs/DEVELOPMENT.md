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
- Default suite and strict Clippy passed. Final crash-recovery check, release
  build, user documentation and fork push are in progress.

## Working rules

Do not use real selection/sign endpoints merely to smoke-test. Use unit tests and
isolated daemon instances for mutations, and real credentials only for read-only
API compatibility checks. Never include credentials or full auth responses here.
