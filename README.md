# BUAA Cli: Powerful tool to Make BUAA Great Again

Are you still struggling with the hassle of classroom check-in?

Are you distressed that Boya course will never be able to grab?

Are you still putting up with the slow, useless of the "智慧北航"?

Now throw it in the trash and try this tool

<p align="center">
  <img src="./assets/image.png"></img>
</p>


> This project is a derivative of [buaa-api](https://github.com/fontlos/buaa-api)

> 注: 本项目只用于学习分享, 请于下载后 24 小时内删除, 使用产生的一切问题由使用者自行承担, 如有侵权我将删除此储存库和软件
>
> Tips: This project is only for learning and sharing, please delete within 24 hours after downloading,
> all problems caused by the use are borne by the user, if there is any infringement I will delete this repository and software

# Features

- ⚡ Fast
  - 🦀 Written in Rust. High-performance data parsing
  - 🎯 Communicates directly with the server via a reverse interface. Reduce the time it takes to transfer static files to open web pages
- ✨ Lightweight: An executable file that is only 5MB in size
- ❤️ Easy
  - 📦Out-of-the-box. No complicated setup required
  - 🎉 Friendly command line output
- 😎 Powerful
  - ⏰ Support for automated operations

# Install

1. Download from [release](https://github.com/fontlos/buaa-cli/release)
2. Install from source:
   ```sh
   git clone https://github.com/fontlos/buaa-cli
   cd buaa-cli
   # Rust 1.89 or newer
   cargo build --release
   ```

# Usage

> It is recommended to add the file path to the environment variable

## SSO Login

Whatever you do, it's the first thing you need to do

For the first time to login:

```sh
buaa login -u <Username> -p <Password>
```

You can also use the previous command to change your username and password

## Boya Course

```sh
# Query the available courses
buaa boya query

# Query all courses
buaa boya query --all

# Query courses with pagination, usually need `-all`
buaa boya query --page 2

# Select a course directly by ID
buaa boya select <ID>

# Drop a course directly by ID
buaa boya drop  <ID>

# If command above print rule, this means you can use this to check-in/out by self
buaa boya check <ID>

# Query selected courses
buaa boya selected

# Query statistics information
buaa boya status
```

## Background task server

The same executable can run a local background server. Existing commands keep
working directly; `boya auto` commands send work to the server and return immediately.

```sh
# Start a detached server (survives closing the terminal)
buaa server start

# Show process status, automation settings, tasks and recent errors
buaa server status

# Stop the server; retain settings and pending tasks for the next start
buaa server off
```

The server listens only on the local loopback interface. Authentication files stay
beside the executable, and server state/logs are in `server/` beside it. Start and
stop are idempotent. Each data directory has at most one server. To use another
directory, set `BUAA_DATA_DIR` for both the server and client commands; log in using
that directory first. The directory must be writable.

`buaa server status` shows the log path. `server/server.log` records JSON Lines
with Unix timestamps in milliseconds. Use the task key and attempt number to
follow dispatch, API calls, Boya results and retries. The log rotates at 2 MiB,
keeping one previous file (`server.log.1`). Startup failures and other process
diagnostics go to `server/server.stderr.log`. Logs record operation names and
safe error categories without storing credentials, coordinates or raw responses.
Logs from before the upgrade may contain older plain-text lines until rotation.

Tasks store absolute Unix timestamps. The scheduler reads the host wall clock at
least every 200 ms while idle, checks it again before dispatch, and rechecks course
windows before submitting an action. Host clock corrections and resume from sleep
therefore update the schedule. Slow discovery queries yield to due actions.
Network/login latency and another in-flight action can still delay an API request;
this is not a hard real-time service. Keep the host awake and its system clock
synchronized. On restart, overdue work runs only while its execution window is
still open; missed signing windows expire.

This starts a resident process, not an OS boot service. After a reboot or crash,
run `buaa server start` again to resume persisted tasks. Interrupted external
requests can be retried: enrollment is reconciled against the selected-course
list, but exactly-once external effects are not guaranteed.

### Boya automation

```sh
buaa server start

# Top up THIS SEMESTER's selected totals to 5 arts and 3 labor courses
# Already selected courses count toward the targets
buaa boya auto select --arts 5 --labor 3

# Also supported: --ethics, --safety, --other
# Update only the supplied categories; retain the other configured targets
buaa boya auto select --arts 6

# Disable one category (does not drop courses already selected)
buaa boya auto select --arts 0

# Toggle continuous automatic check-in AND check-out
buaa boya auto sign

# Schedule check-in/out for one already selected self-service course
buaa boya auto sign <ID>

buaa server status
```

Automatic selection scans all available course pages and only enrolls courses with
a non-empty `sign_config`. It waits for selection to open, checks semester counts
before every enrollment, and verifies the selected list after submitting. Configured
targets remain active, so newly available courses and later quota shortages are
discovered on subsequent scans. Set each configured category to `0` to stop it.

Both policies start with an immediate scan when enabled and schedule each
subsequent scan 30 minutes after the previous successful scan. Scheduled
course actions still run at their own due times.

Continuous signing discovers all selected self-service courses,
including courses selected later. It schedules separate check-in and check-out
tasks within their respective windows. Selection and signing are independent:
enable automatic signing separately if you want newly enrolled courses signed.
Turning continuous signing off cancels its pending work; explicit `<ID>` tasks
remain scheduled. Single-course submissions are validated asynchronously; inspect
`server status` for planning failures or expired windows.

Network and authentication failures use bounded retry backoff. Windowed actions
retry until their window closes; one-off planning stops after 10 failed attempts
and can be resubmitted. If login needs a CAPTCHA, the server records an error
instead of waiting on stdin. Run `buaa login` interactively using the same data
directory; the next task attempt reloads the new credentials. If that option is
needed for background work, stop the server and start it with
`buaa --disable-tls server start`.

For contributors, see [AGENTS.md](AGENTS.md) and [development notes](docs/DEVELOPMENT.md).

## Smart Classroom

```sh
# Automatically check in for today's lessons
buaa class auto

# Query one day's schedule, format: YYYYMMDD
buaa class query <Date>

# Query the courses for this term
# eg. '202420251' means the first term of 2024-2025
buaa class query <Term ID>

# Query schedules of one course. ID len is 5
buaa class query <Course ID>

# Check in directly by schedule ID
# It is even possible to check in for the past schedule
buaa class checkin <Schedule ID>

# Checkin for some day. format: YYYYMMDD
buaa class checkin <Date>
```

## Teacher Evaluation System

**Warning!**: Due to the poor design of the evaluation system server,
using this may cause the evaluation button on the web page to become unclickable.
But don't worry, the evaluation data has been submitted correctly.
If you want to view the evaluation results on the web page,
you can remove the 'disabled' attribute of the button in the browser console,
and you'll be able to click it.
Or you might wait a little longer, and it may return to normal.

```sh
# List the forms that need to be filled out and use the index to fill out the specified form
buaa tes list

# Automatically fill all forms
buaa tes auto
```

## WiFi

You can use the following simple command to (dis)connect to **BUAA-WiFi**

```sh
buaa wifi login
buaa wifi logout
```

On the Windows platform, you can use a `.bat` file
and add it to the `C:\Users\<Username>\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup` folder,
so that you can automatically connect to WiFi at boot

## Other

Sometimes Server's SSL certificate may be invalid, you can use `--disable-tls`

```sh
buaa --disable-tls [COMMAND]
```
