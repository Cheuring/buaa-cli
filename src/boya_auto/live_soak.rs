//! Opt-in real-account check of the scheduler and both Boya discovery paths.

use super::*;
use crate::server::{self, protocol, storage};
use std::time::{Duration, Instant};

struct ReadOnlyBackend(buaa_api::Context);

impl Backend for ReadOnlyBackend {
    async fn selected(&self) -> Result<Vec<buaa_api::api::boya::Selected>, String> {
        self.0.selected().await
    }

    async fn courses(&self, page: u8) -> Result<Vec<Course>, String> {
        self.0.courses(page).await
    }

    async fn course(&self, id: u32) -> Result<Course, String> {
        self.0.course(id).await
    }

    async fn enroll(&self, _: u32) -> Result<(), String> {
        Err("Disabled in read-only soak test".into())
    }

    async fn sign(
        &self,
        _: u32,
        _: &buaa_api::api::boya::Coordinate,
        _: bool,
    ) -> Result<buaa_api::api::boya::SignInfo, String> {
        Err("Disabled in read-only soak test".into())
    }
}

struct ReadOnlyHandler(ReadOnlyBackend);

impl Handler for ReadOnlyHandler {
    fn configure(&self, _: &mut State, _: &str, _: Value) -> Result<String, String> {
        Err("Only read-only discovery is enabled in this instance".into())
    }

    fn submit(&self, _: &mut State, _: &str, _: Value) -> Result<String, String> {
        Err("Only read-only discovery is enabled in this instance".into())
    }

    async fn execute(&self, task: Task, _: Value) -> Result<Outcome, String> {
        let mut outcome = match task.kind.as_str() {
            "boya.auto.select.discover" => {
                let targets = ["arts", "ethics", "labor", "safety", "other"]
                    .into_iter()
                    .map(|key| (key.to_owned(), u32::MAX))
                    .collect();
                select_plan(
                    &self.0,
                    &Policy {
                        targets,
                        ..Policy::default()
                    },
                )
                .await?
            }
            "boya.auto.sign.discover" => sign_discover(&self.0, 0).await?,
            _ => return Err("Refusing to execute a course action in a read-only test".into()),
        };
        task_event(
            "info",
            "soak.discovery",
            json!({"planned_actions": outcome.tasks.len()}),
        );
        // Exercise real discovery and scheduling calculations without committing
        // any derived enrollment, check-in or check-out tasks to the queue.
        outcome.tasks.clear();
        Ok(outcome)
    }
}

#[tokio::test]
#[ignore = "requires an existing real account, university network, and two hours"]
async fn live_read_only_daemon_soak() {
    let auth_dir =
        PathBuf::from(std::env::var_os("BUAA_LIVE_AUTH_DIR").expect("Set BUAA_LIVE_AUTH_DIR"));
    let data_dir = PathBuf::from(
        std::env::var_os("BUAA_SOAK_DATA_DIR").expect("Set BUAA_SOAK_DATA_DIR to a new directory"),
    );
    std::fs::create_dir(&data_dir).expect("Use a fresh isolated directory for the soak test");
    let dir = server::directory(&data_dir);
    storage::private_dir(&dir).unwrap();
    // Keep authentication in memory. The source account files are never saved
    // or copied to the isolated runtime directory.
    let session = crate::auth::Session::load(&auth_dir, false, false).expect("Load existing auth");
    let mut state = State::default();
    state.enqueue(discover(SELECT));
    state.enqueue(discover(SIGN));
    storage::write_json(&dir.join("state.json"), &state).unwrap();

    let mut daemon = tokio::spawn(async move {
        server::run(&data_dir, ReadOnlyHandler(ReadOnlyBackend(session.context))).await
    });
    let started = Instant::now();
    let duration = Duration::from_secs(2 * 60 * 60);
    let mut checks = 0;
    let mut next_check = Duration::ZERO;
    loop {
        let status = protocol::call(&dir, protocol::Request::Status).await;
        match status {
            Ok(response) if response.ok => {
                checks += 1;
                println!(
                    "soak health: elapsed_s={} runner=up",
                    started.elapsed().as_secs()
                );
            }
            Err(_) if checks == 0 && started.elapsed() < Duration::from_secs(5) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            _ => panic!("Soak server not responding; inspect the isolated server log"),
        }
        if started.elapsed() >= duration {
            break;
        }
        next_check = (next_check + Duration::from_secs(300)).min(duration);
        tokio::select! {
            stopped = &mut daemon => panic!("Soak server exited early: {stopped:?}"),
            _ = tokio::time::sleep_until((started + next_check).into()) => {},
        }
    }
    assert!(
        protocol::call(&dir, protocol::Request::Stop)
            .await
            .unwrap()
            .ok
    );
    daemon.await.unwrap().unwrap();
    let log = std::fs::read_to_string(dir.join("server.log")).unwrap();
    let events: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for key in ["boya.auto.select.discover", "boya.auto.sign.discover"] {
        let completed = events
            .iter()
            .filter(|event| event["event"] == "soak.discovery" && event["task"]["key"] == key)
            .count();
        println!("soak discovery: {key} successful_rounds={completed}");
        assert!(
            completed >= 4,
            "Expected at least four successful {key} rounds"
        );
    }
    println!(
        "soak complete: elapsed_s={} responsive_checks={checks}",
        started.elapsed().as_secs()
    );
}
