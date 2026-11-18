# BUAA Cli: Powerful tool to Make BUAA Great Again

## [Unreleased]

- Feat
  - Add a local resident task server with persistent wall-clock scheduling,
    restart recovery, retry backoff, status reporting and detached startup.
  - Add `boya auto select` to fill semester category targets using self-service
    courses only, and `boya auto sign [ID]` for automatic check-in/out. Both
    discover courses every 30 minutes after an initial scan.
  - Add `BUAA_DATA_DIR` for isolated authentication and server instances.
  - Record task attempts, API calls, confirmed Boya results and scheduled
    retries in a rotating server log.
  - Schedule background signing about five minutes before class starts or
    ends, constrained by the school's window, with up to 30 seconds of jitter.
- Fix
  - Protect concurrent CLI/server authentication updates with short file locks,
    atomic writes and stale-session checks.
- Chore
  - Document automation behavior and ongoing AI development context.
  - Require Rust 1.89 or newer for portable OS file locks.

## [v0.5.0] - 2026-11-15

- Feat
  - (Breaking) Adapt login when not on campus. Need pass captcha which will be wrote in binary path
- Fix
  - Fix SSO login failed when not on campus. Detail see https://github.com/fontlos/buaa-api/issues/318
- Chore
  - Update `buaa_api`
  - Update other dependencies

## [v0.4.5] - 2026-09-24

- Fix
  - Fix `buaa tes` command
- Chore
  - Update `buaa_api`

## [v0.4.4] - 2026-09-15

- Fix
  - Fix VPN mode credentials being refreshed too late for `buaa class`
- Chore
  - Update `buaa_api`
  - Update other dependencies

## [v0.4.3] - 2026-09-08

- Fix
  - `class query` adapt more id formats. After 2026.09, the term ID will have 10 digits, e.g. `2026202701` is 2026 autumn term, and course id can have 6 digits

## [v0.4.2] - 2026-06-06

- Fix
  - Fix VPN mode for `buaa class`
- Chore
  - Update `buaa_api`
  - Update other dependencies

## [v0.4.1] - 2026-06-01

- Fix(Breaking)
  - `class` command break caused by server update
    - The login and checkin URL path inexplicably changed again, and `Sessionid` is once again a required request header
    - This break VPN mode for `buaa class`
- Chore
  - Update `buaa_api`
  - Update other dependencies

## [v0.4.0] - 2026-05-27

- Feat
  - Add automated VPN mode for `buaa class` (experimental)
- Refactor
  - New method to detect WiFi
- Chore
  - Update `buaa_api`
  - Update other dependencies

## [v0.3.8] - 2026-04-20

- Feat
  - Add `buaa exam` command to query exam schedule

## [v0.3.7] - 2026-04-12

- Fix
  - `wifi` command break caused by server update

## [v0.3.6] - 2026-04-07

- Fix
  - Time overflow in `boya` or `class`, which cause waiting for almost `u64::MAX` seconds

## [v0.3.5] - 2026-03-29

- Fix
  - `class` command is broken due to server bad update
    - Now `class checkin` is based on server internal time which is not standard UTC
- Chore
  - Update `buaa_api`
  - Update other dependencies

## [v0.3.4] - 2025-12-31

- Fix
  - `tes` command now work. But there are some **WARNINGS**
    > **Warning!**: Due to the poor design of the evaluation system server,
    > using this may cause the evaluation button on the web page to become unclickable.
    > But don't worry, the evaluation data has been submitted correctly.
    > If you want to view the evaluation results on the web page,
    > you can remove the 'disabled' attribute of the button in the browser console,
    > and you'll be able to click it.
    > Or you might wait a little longer, and it may return to normal.
- Chore
  - Update `buaa_api`
  - Cargo fmt

## [v0.3.3] - 2025-12-30

- Fix
  - `class` command is broken due to server bad update
    - Server NGINX was misconfigured, causing all `/app/` paths on port `8346` to be mounted under `/app/app/`. And change to port `8347` to bypass
- Chore
  - Update `buaa_api`

## [v0.3.2] - 2025-12-29

- Feat
  - Add `CanCheck` field in `buaa boya query` print table
  - `buaa boya check` can waiting for check-in/out
  - Add `--page` for `buaa boya query` for pagination
- Remove
  - `buaa boya rule`
- Refactor
  - Move waiting logic from `buaa boya query` to `buaa boya select`
  - Use RNG from `buaa_api`
  - Split `buaa boya selected` and `buaa boya status`
- Chore
  - Update `buaa_api`

## [v0.3.1] - 2025-12-19

- Fix:
  - `boya::SignInfo` parse failed
- Chore
  - Update `buaa-api`

## [v0.3.0] - 2025-12-17

- Refactor(BREAKING)
  - Update `buaa-api`, use new cookie store. Need relogin
- Fix
  - `boya::SignRule` parse failed (cause by server update)

## [v0.2.1] - 2025-11-21

- Fix
  - Add `disable-tls` flag to disable TLS if necessary. Sometimes server SSL cred maybe invalid

## [v0.2.0] - 2025-10-26

- Feat
  - Add `buaa boya rule <ID>` to query sign rule
  - Add `buaa boya check <ID>` to check-in/out
  - Add `buaa class query <Date>` to query schedule of some day before class begin
  - Now `buaa class auto` can checkin before class begin
  - Now `buaa class checkin <ID>` can checkin before class begin
  - Add `buaa class checkin <Date>`
- Remove
  - `Campus::Other` in Boya
- Chore
  - Update `buaa-api`

## [v0.1.4] - 2025-9-25

- Feat
  - Now we needn't `login` except `sso` and `wifi`
- Fix
  - `class` login (cause by server update)
  - More accurate course matching for `class auto` (cause by server update)
  - `spoc::Schedule` parse failed (cause by server update)
  - `tes` command

## [v0.1.3] - 2025-6-13

- Feat
  - Now we sub-api will auto call `sso.login`

## [v0.1.2] - 2025-1-15

- Feat
  - Add teacher evaluation system command
    - `buaa evaluaton list`: list and fill special form
    - `buaa evaluation fill`: fill all form one by one
- Refactor
  - Print table

## [v0.1.1] - 2024-12-8

- Feat
  - Add `buaa boya status` to query statistics information
  - Auto relogin for `buaa boya query` and `buaa boya status`
- Fix
  - Can't query selected course when course is full

## [v0.1.0] - 2024-11-28

- Feat
  - WiFi Login
  - Smart Classroom Checkin
  - Boya Course automated Select
