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
        Command::new(env!("CARGO_BIN_EXE_buaa"))
            .env("BUAA_DATA_DIR", &self.0)
            .args(args)
            .output()
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
