//! Process-level lifecycle checks with no credentials and no university requests.
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Instance(PathBuf);
impl Instance {
    fn new() -> Self {
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let path =
            std::env::temp_dir().join(format!("buaa-cli-test-{:x}", u64::from_ne_bytes(random)));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn command(&self, args: &[&str]) -> Output {
        let directory = self.0.clone();
        let arguments: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let output = Command::new(env!("CARGO_BIN_EXE_buaa"))
                .env("BUAA_DATA_DIR", directory)
                .args(arguments)
                .output();
            let _ = send.send(output);
        });
        receive
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("CLI process or captured output did not finish within 30 seconds")
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.command(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.0.join("server/state.json")).unwrap()).unwrap()
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.command(&["server", "off"]);
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_bark_config_stays_private_and_can_pause_without_the_daemon() {
    use std::io::Write;
    use std::process::Stdio;

    let instance = Instance::new();
    assert!(
        instance
            .ok(&["notify", "status"])
            .contains("not configured")
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_buaa"))
        .env("BUAA_DATA_DIR", &instance.0)
        .args(["notify", "bark", "set"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"https://api.day.app/example-device-key\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("example-device-key"));
    let config_file = instance.0.join("server/notify.json");
    assert!(config_file.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            config_file.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    instance.ok(&["notify", "off"]);
    assert!(instance.ok(&["notify", "status"]).contains("paused"));
    instance.ok(&["notify", "on"]);
    let status = instance.ok(&["notify", "status"]);
    assert!(status.contains("enabled"));
    assert!(!status.contains("example-device-key"));
    assert!(!instance.0.join("server/state.json").exists());
}

#[test]
fn cli_lifecycle_and_durable_configuration() {
    let instance = Instance::new();
    assert!(instance.ok(&["server", "status"]).contains("Stopped"));
    assert!(!instance.command(&["boya", "auto", "sign"]).status.success());
    assert!(instance.ok(&["server", "start"]).contains("Started"));
    assert!(
        instance
            .ok(&["server", "start"])
            .contains("Already running")
    );
    assert!(instance.ok(&["server", "status"]).contains("Running"));
    // Zero disables a category and schedules no work or network requests.
    instance.ok(&["boya", "auto", "select", "--arts", "0", "--labor", "0"]);
    assert_eq!(instance.state()["settings"]["boya"]["targets"], json!({}));
    assert!(
        !instance
            .command(&["boya", "auto", "sign", "0"])
            .status
            .success()
    );
    assert!(instance.ok(&["server", "off"]).contains("Stopped"));
    assert!(!instance.0.join("server/endpoint.json").exists());
    // Restart immediately after stop: the acknowledgment releases the OS lock.
    instance.ok(&["server", "start"]);
    assert_eq!(instance.state()["settings"]["boya"]["auto_sign"], false);
    instance.ok(&["server", "off"]);
    assert!(instance.ok(&["server", "status"]).contains("Stopped"));
    instance.ok(&["server", "off"]);
    assert!(!instance.0.join("cred.json").exists());
    assert!(!instance.0.join("cookies.json").exists());
}

#[test]
fn task_log_correlates_failed_attempt_with_scheduled_retry() {
    use std::time::{Duration, Instant};

    let instance = Instance::new();
    instance.ok(&["server", "start"]);
    // No credentials: planning stops at local auth loading, before any API call.
    instance.ok(&["boya", "auto", "sign", "123"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let path = instance.0.join("server/server.log");
    loop {
        let contents = fs::read_to_string(&path).unwrap();
        let events: Vec<Value> = contents
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let first_attempt = |event: &&Value| {
            event["task"]["key"] == "boya.sign.plan:123" && event["task"]["attempt"] == 1
        };
        if let Some(completed) = events
            .iter()
            .filter(first_attempt)
            .find(|event| event["event"] == "task.finished")
        {
            let started = events
                .iter()
                .filter(first_attempt)
                .find(|event| event["event"] == "task.started")
                .unwrap();
            assert!(
                started["details"]["due_ms"].as_i64().unwrap()
                    <= started["timestamp_ms"].as_i64().unwrap()
            );
            assert_eq!(completed["level"], "warn");
            assert_eq!(completed["details"]["status"], "pending");
            assert!(
                completed["details"]["error"]
                    .as_str()
                    .unwrap()
                    .contains("buaa login")
            );
            assert!(
                completed["details"]["next_due_ms"].as_i64().unwrap()
                    > completed["timestamp_ms"].as_i64().unwrap()
            );
            assert!(
                !events
                    .iter()
                    .any(|event| event["event"] == "boya.request.started")
            );
            break;
        }
        assert!(Instant::now() < deadline, "Missing task events: {contents}");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(instance.ok(&["server", "status"]).contains("Running"));
}

#[cfg(unix)]
#[test]
fn process_crash_recovers_pending_tasks() {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let instance = Instance::new();
    fs::create_dir_all(instance.0.join("server")).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let state = json!({"version": 1, "settings": {}, "tasks": [{
        "key": "recover-after-crash", "kind": "fixture-never-executed", "payload": {},
        "due_ms": now + 60_000, "expires_ms": now + 120_000,
        "group": null, "max_attempts": 10, "status": "running", "attempts": 1,
        "last_error": null
    }]});
    fs::write(
        instance.0.join("server/state.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    instance.ok(&["server", "start"]);
    assert_eq!(instance.state()["tasks"][0]["status"], "pending");
    let endpoint: Value =
        serde_json::from_slice(&fs::read(instance.0.join("server/endpoint.json")).unwrap())
            .unwrap();
    let pid = endpoint["pid"].as_i64().unwrap() as i32;
    // This is the detached child started by this test, never an existing server.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    for _ in 0..250 {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(instance.0.join("server/server.lock"))
            .unwrap();
        if file.try_lock().is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    instance.ok(&["server", "start"]);
    assert_eq!(instance.state()["tasks"][0]["status"], "pending");
    assert_eq!(instance.state()["tasks"].as_array().unwrap().len(), 1);
    instance.ok(&["server", "off"]);
}
