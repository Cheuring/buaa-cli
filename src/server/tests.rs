use super::*;
use serde_json::{Value, json};
use std::sync::Mutex;

#[derive(Clone)]
struct MockHandler(Arc<Mutex<Vec<(String, i64)>>>);
impl Handler for MockHandler {
    fn configure(
        &self,
        state: &mut State,
        namespace: &str,
        payload: Value,
    ) -> Result<String, String> {
        if namespace != "mock" {
            return Err("Unknown namespace".into());
        }
        state.settings = payload;
        Ok("Configured".into())
    }
    fn submit(&self, state: &mut State, kind: &str, payload: Value) -> Result<String, String> {
        if kind != "mock" {
            return Err("Unknown task kind".into());
        }
        let key = payload["key"].as_str().ok_or("Missing key")?.to_string();
        state.enqueue(Task::new(key, kind, payload, now_ms()));
        Ok("Submitted".into())
    }
    async fn execute(&self, task: Task, _settings: Value) -> Result<Outcome, String> {
        if task.kind == "slow" {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        self.0.lock().unwrap().push((task.key, now_ms()));
        Ok(Outcome::default())
    }
}

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let path =
            std::env::temp_dir().join(format!("buaa-server-test-{:x}", u64::from_ne_bytes(random)));
        Self(path)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn ready(dir: &Path) {
    for _ in 0..100 {
        if protocol::call(dir, Request::Status).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("Test server did not start");
}

#[tokio::test]
async fn durable_dispatch_preemption_and_ipc() {
    let temp = TestDir::new();
    let dir = directory(&temp.0);
    storage::private_dir(&dir).unwrap();
    let records = Arc::new(Mutex::new(Vec::new()));
    let mut state = State::default();
    let due = now_ms() + 500;
    let mut slow = Task::new("discovery".into(), "slow", json!({}), now_ms());
    slow.priority = 0;
    slow.preemptible = true;
    slow.status = Status::Running; // simulate a crash during discovery
    state.enqueue(slow);
    state.enqueue(Task::new("deadline".into(), "mock", json!({}), due));
    let mut missed = Task::new("missed".into(), "mock", json!({}), due - 2_000);
    missed.expires_ms = Some(due - 1_000);
    state.enqueue(missed);
    storage::write_json(&dir.join("state.json"), &state).unwrap();
    let server = tokio::spawn({
        let path = temp.0.clone();
        let handler = MockHandler(records.clone());
        async move { run(&path, handler).await }
    });
    ready(&dir).await;
    // OS locking rejects a second instance; status remains responsive while
    // a slow handler is running in the first instance.
    assert!(storage::lock(&dir).is_err());
    assert!(protocol::call(&dir, Request::Status).await.unwrap().ok);
    let endpoint: Endpoint = storage::read_json(&dir.join("endpoint.json")).unwrap();
    let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, endpoint.port))
        .await
        .unwrap();
    protocol::write_message(
        &mut stream,
        &Envelope {
            token: "wrong".into(),
            request: Request::Stop,
        },
    )
    .await
    .unwrap();
    assert!(
        !protocol::read_message::<Response>(&mut stream)
            .await
            .unwrap()
            .ok
    );
    assert!(
        !protocol::call(
            &dir,
            Request::Submit {
                kind: "invalid".into(),
                payload: json!({})
            }
        )
        .await
        .unwrap()
        .ok
    );
    for _ in 0..100 {
        if records
            .lock()
            .unwrap()
            .iter()
            .any(|(key, _)| key == "deadline")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let executed = records.lock().unwrap().clone();
    let timestamp = executed
        .iter()
        .find(|(key, _)| key == "deadline")
        .unwrap()
        .1;
    assert!(timestamp >= due, "Task executed early");
    assert!(timestamp < due + 1_000, "Discovery blocked timed dispatch");
    assert!(!executed.iter().any(|(key, _)| key == "missed"));
    assert!(
        protocol::call(
            &dir,
            Request::Configure {
                namespace: "mock".into(),
                payload: json!({"saved": true})
            }
        )
        .await
        .unwrap()
        .ok
    );
    assert!(protocol::call(&dir, Request::Stop).await.unwrap().ok);
    server.await.unwrap().unwrap();
    assert!(!dir.join("endpoint.json").exists());
    let saved = storage::load_state(&dir).unwrap();
    assert_eq!(saved.settings, json!({"saved": true}));
    assert_eq!(
        saved
            .tasks
            .iter()
            .find(|t| t.key == "deadline")
            .unwrap()
            .status,
        Status::Succeeded
    );
    assert_eq!(
        saved
            .tasks
            .iter()
            .find(|t| t.key == "missed")
            .unwrap()
            .status,
        Status::Expired
    );
    assert_eq!(
        saved
            .tasks
            .iter()
            .find(|t| t.key == "discovery")
            .unwrap()
            .status,
        Status::Pending
    );
    // Restart does not replay the completed task and keeps the durable settings.
    let restarted = tokio::spawn({
        let path = temp.0.clone();
        let handler = MockHandler(records.clone());
        async move { run(&path, handler).await }
    });
    ready(&dir).await;
    let status = protocol::call(&dir, Request::Status)
        .await
        .unwrap()
        .state
        .unwrap();
    assert_eq!(status.settings, json!({"saved": true}));
    protocol::call(&dir, Request::Stop).await.unwrap();
    restarted.await.unwrap().unwrap();
    assert_eq!(
        records
            .lock()
            .unwrap()
            .iter()
            .filter(|(key, _)| key == "deadline")
            .count(),
        1
    );
}

#[test]
fn retry_backoff_recovery_and_storage_validation() {
    let temp = TestDir::new();
    storage::private_dir(&temp.0).unwrap();
    let mut state = State::default();
    let mut task = Task::new("retry".into(), "mock", json!({}), 100);
    task.attempts = 1;
    task.status = Status::Running;
    task.expires_ms = Some(5_000);
    state.enqueue(task.clone());
    finish(&mut state, "retry", Err("temporary".into()), 100);
    assert_eq!(state.tasks[0].due_ms, 2_100);
    assert_eq!(state.tasks[0].status, Status::Pending);
    // Rediscovery cannot erase a retry's backoff.
    state.enqueue(task);
    assert_eq!(state.tasks[0].due_ms, 2_100);
    state.tasks[0].attempts = 3;
    finish(&mut state, "retry", Err("temporary".into()), 100);
    assert_eq!(state.tasks[0].status, Status::Expired);
    storage::write_json(&temp.0.join("state.json"), &state).unwrap();
    assert_eq!(storage::load_state(&temp.0).unwrap().tasks.len(), 1);
    state.version = 999;
    storage::write_json(&temp.0.join("state.json"), &state).unwrap();
    assert!(storage::load_state(&temp.0).is_err());
    fs::write(temp.0.join("state.json"), "corrupt").unwrap();
    assert!(storage::load_state(&temp.0).is_err());
}
