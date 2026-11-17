# Development guide

This is a Rust CLI for BUAA services. Keep existing interactive commands working.
Read `docs/DEVELOPMENT.md` for current scope, decisions, verification and next steps.

- Work on `codex/background-task-server`; upstream is `origin`, the user's fork is
  `self_fork`. The user authorized incremental commits and pushes to their fork.
- Keep scheduling and IPC independent of Boya. Put domain planning/execution in
  the Boya automation module, not in the scheduler.
- Persist absolute Unix timestamps; recheck the host wall clock before dispatch.
  Never implement a long-lived schedule with one relative sleep.
- Do not print, commit or copy authentication material into reports. Auth files
  live beside the executable by default. Use an isolated `BUAA_DATA_DIR` in tests.
- Background execution must never read stdin for a CAPTCHA. Report an actionable
  authentication error and retry later instead.
- Use `cargo fmt --check`, `cargo test --offline`, and `cargo clippy --offline
  --all-targets -- -D warnings`. Verify server lifecycle with an isolated data dir.
- Real-account read-only queries are authorized. Do not enroll, drop or sign for
  real courses as a test without a specific course/action request from the user.
- Commit coherent milestones, and update `docs/DEVELOPMENT.md` when behavior,
  validation or remaining work changes. Keep credentials/runtime state ignored.
