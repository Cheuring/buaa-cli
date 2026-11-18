//! Local daemon infrastructure. Domain behavior is supplied by a Handler.

pub(crate) mod logging;
pub mod protocol;
pub mod storage;
pub mod task;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;

use logging::{Log, open_append};
use protocol::{Endpoint, Envelope, Request, Response};
use serde_json::json;
use std::fs;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;
use task::{Outcome, State, Status, Task, now_ms};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

pub trait Handler: Send + Sync + 'static {
    /// Revalidate newly produced work against settings changed during execution.
    fn reconcile(&self, _state: &mut State) {}
    fn configure(
        &self,
        state: &mut State,
        namespace: &str,
        payload: serde_json::Value,
    ) -> Result<String, String>;
    fn submit(
        &self,
        state: &mut State,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<String, String>;
    fn execute(
        &self,
        task: Task,
        settings: serde_json::Value,
    ) -> impl Future<Output = Result<Outcome, String>> + Send;
}

pub fn directory(data_dir: &Path) -> PathBuf {
    data_dir.join("server")
}

pub async fn client(data_dir: &Path, request: Request) -> io::Result<()> {
    let dir = directory(data_dir);
    let status = matches!(request, Request::Status);
    let stop = matches!(request, Request::Stop);
    let response = match protocol::call(&dir, request).await {
        Ok(response) => response,
        Err(_) if (status || stop) && (!dir.exists() || storage::lock(&dir).is_ok()) => Response {
            ok: true,
            message: "Stopped; run `buaa server start` to resume pending tasks".into(),
            state: if status {
                Some(storage::load_state(&dir)?)
            } else {
                None
            },
        },
        Err(error) => return Err(error),
    };
    if !response.ok {
        return Err(io::Error::other(response.message));
    }
    println!("[Info]::<Server>: {}", response.message);
    if let Some(state) = response.state {
        println!("Settings: {}", state.settings);
        let mut builder = tabled::builder::Builder::new();
        builder.push_record(["Task", "State", "Due (UTC+8)", "Attempts", "Last error"]);
        for task in &state.tasks {
            builder.push_record([
                task.key.clone(),
                format!("{:?}", task.status),
                buaa_api::time::DateTime::from_timestamp(task.due_ms / 1000).format(),
                task.attempts.to_string(),
                task.last_error.clone().unwrap_or_default(),
            ]);
        }
        crate::utils::print_table(builder);
        println!("Log: {}", directory(data_dir).join("server.log").display());
    }
    Ok(())
}

pub async fn start(data_dir: &Path, disable_tls: bool) -> io::Result<()> {
    let dir = directory(data_dir);
    storage::private_dir(&dir)?;
    if let Ok(response) = protocol::call(&dir, Request::Status).await
        && response.ok
    {
        println!("[Info]::<Server>: Already running");
        return Ok(());
    }
    // Reject a held lock even when a daemon is still starting or unhealthy.
    drop(storage::lock(&dir).map_err(|_| {
        io::Error::other("Server is already starting or running; inspect server.log")
    })?);
    // Reserve stderr for failures outside the event logger (such as startup
    // errors and panics). Never hold server.log open in a detached stdio handle.
    let diagnostics = open_append(&dir.join("server.stderr.log"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .env("BUAA_DATA_DIR", data_dir)
        .args(["server", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(diagnostics);
    if disable_tls {
        command.arg("--disable-tls");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New session survives terminal exit. Only async-signal-safe setsid runs
        // between fork and exec; all allocation/configuration happens above.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000008 | 0x00000200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    #[cfg(windows)]
    let inherited_stdio = windows::InheritanceGuard::disable_stdio()?;
    let child = command.spawn();
    #[cfg(windows)]
    drop(inherited_stdio);
    let mut child = child?;
    for _ in 0..100 {
        if let Ok(response) = protocol::call(&dir, Request::Status).await
            && response.ok
        {
            println!("[Info]::<Server>: Started (PID {})", child.id());
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "Server exited ({status}); inspect {}",
                dir.join("server.stderr.log").display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Do not leave an unreported child after a failed startup handshake.
    let _ = child.kill();
    let _ = child.wait();
    Err(io::Error::other(
        "Server startup timed out; inspect server.log and server.stderr.log",
    ))
}

type Incoming = (Request, oneshot::Sender<Response>);
type Worker = JoinHandle<Result<Outcome, String>>;

pub async fn run<H: Handler>(data_dir: &Path, handler: H) -> io::Result<()> {
    let dir = directory(data_dir);
    storage::private_dir(&dir)?;
    let _lock = storage::lock(&dir)?;
    let log = Log::new(dir.join("server.log"));
    let mut state = storage::load_state(&dir)?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
    let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let endpoint = Endpoint {
        port: listener.local_addr()?.port(),
        pid: std::process::id(),
        token,
    };
    storage::write_json(&dir.join("state.json"), &state)?;
    storage::write_json(&dir.join("endpoint.json"), &endpoint)?;
    let _endpoint_guard = EndpointGuard(dir.join("endpoint.json"));
    log.event("info", "server.started", json!({"pid": endpoint.pid}));
    let (tx, mut rx) = mpsc::channel::<Incoming>(32);
    let listener_task = tokio::spawn(accept(listener, endpoint.token, tx));
    let handler = Arc::new(handler);
    let mut active: Option<(Task, Worker, Instant)> = None;
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        let expired = state.expire(now_ms());
        if !expired.is_empty() {
            for task in expired {
                log.task(
                    "warn",
                    "task.expired",
                    task,
                    json!({"expires_ms": task.expires_ms}),
                );
            }
            state.compact(now_ms());
            storage::write_json(&dir.join("state.json"), &state)?;
        }
        if let Some((task, _, _)) = &active
            && task.preemptible
            && state
                .next_due(now_ms())
                .is_some_and(|index| state.tasks[index].priority > task.priority)
        {
            let (task, worker, _) = active.take().unwrap();
            worker.abort();
            let _ = worker.await;
            if let Some(interrupted) = state.tasks.iter_mut().find(|t| t.key == task.key) {
                interrupted.status = Status::Pending;
                interrupted.due_ms = now_ms() + 1_000;
                log.task(
                    "info",
                    "task.preempted",
                    &task,
                    json!({"next_due_ms": interrupted.due_ms}),
                );
            }
        }
        if active.is_none()
            && let Some(index) = state.next_due(now_ms())
        {
            state.tasks[index].status = Status::Running;
            state.tasks[index].attempts += 1;
            storage::write_json(&dir.join("state.json"), &state)?;
            let task = state.tasks[index].clone();
            let settings = state.settings.clone();
            let executor = handler.clone();
            let work = task.clone();
            log.task(
                "info",
                "task.started",
                &task,
                json!({
                    "due_ms": task.due_ms, "expires_ms": task.expires_ms
                }),
            );
            let worker = tokio::spawn(log.in_task(&task, async move {
                timeout(Duration::from_secs(45), executor.execute(work, settings))
                    .await
                    .unwrap_or_else(|_| Err("Task timed out; will reconcile before retry".into()))
            }));
            active = Some((task, worker, Instant::now()));
        }
        let wait = if active.is_some() {
            Duration::from_millis(200)
        } else {
            state.wait_duration(now_ms())
        };
        tokio::select! {
            _ = &mut shutdown => break,
            Some((request, reply)) = rx.recv() => {
                let stop = matches!(request, Request::Stop);
                let operation = match &request {
                    Request::Status => None,
                    Request::Stop => Some("stop"),
                    Request::Configure { .. } => Some("configure"),
                    Request::Submit { .. } => Some("submit"),
                };
                let mut next = state.clone();
                let response = match request {
                    Request::Status => Response { ok: true, message: format!("Running (PID {})", std::process::id()), state: Some(state.clone()) },
                    Request::Stop => Response::ok("Stopped; pending tasks are retained"),
                    Request::Configure { namespace, payload } => match handler.configure(&mut next, &namespace, payload) {
                        Ok(message) => Response::ok(message), Err(error) => Response::error(error),
                    },
                    Request::Submit { kind, payload } => match handler.submit(&mut next, &kind, payload) {
                        Ok(message) => Response::ok(message), Err(error) => Response::error(error),
                    },
                };
                if response.ok {
                    storage::write_json(&dir.join("state.json"), &next)?;
                    state = next;
                    if let Some((task, _, _)) = &active
                        && !state.tasks.iter().any(|t| t.key == task.key && t.status == Status::Running) {
                        let (task, worker, _) = active.take().unwrap();
                        worker.abort();
                        let _ = worker.await;
                        log.task("info", "task.cancelled", &task, json!({"reason": "configuration_or_submission"}));
                    }
                }
                if let Some(operation) = operation {
                    log.event(if response.ok { "info" } else { "warn" }, "ipc.command",
                        json!({"operation": operation, "accepted": response.ok}));
                }
                if stop {
                    // Await current cancellation and release endpoint/lock before
                    // acknowledging, so a following `start` cannot see a ghost.
                    if let Some((task, worker, _)) = active.take() {
                        worker.abort();
                        let _ = worker.await;
                        log.task("info", "task.interrupted", &task, json!({"reason": "shutdown"}));
                    }
                    state.recover();
                    storage::write_json(&dir.join("state.json"), &state)?;
                    listener_task.abort();
                    log.event("info", "server.stopped", json!({"reason": "command"}));
                    drop(_endpoint_guard);
                    drop(_lock);
                    let _ = reply.send(response);
                    return Ok(());
                }
                let _ = reply.send(response);
            },
            result = async { (&mut active.as_mut().unwrap().1).await }, if active.is_some() => {
                let (task, _, started) = active.take().unwrap();
                let elapsed_ms = started.elapsed().as_millis() as u64;
                let result = result.unwrap_or_else(|error| {
                    let reason = if error.is_panic() { "panicked" } else { "cancelled" };
                    Err(format!("Task worker {reason}"))
                });
                let planned_tasks = result.as_ref().ok().map(|outcome| outcome.tasks.len());
                finish(&mut state, &task.key, result, now_ms());
                if let Some(completed) = state.tasks.iter().find(|t| t.key == task.key) {
                    let next_due_ms = (completed.status == Status::Pending).then_some(completed.due_ms);
                    log.task(
                        if completed.last_error.is_some() { "warn" } else { "info" },
                        "task.finished",
                        &task,
                        json!({"status": completed.status, "elapsed_ms": elapsed_ms,
                            "next_due_ms": next_due_ms, "error": completed.last_error,
                            "planned_tasks": planned_tasks}),
                    );
                }
                handler.reconcile(&mut state);
                state.compact(now_ms());
                storage::write_json(&dir.join("state.json"), &state)?;
            },
            _ = tokio::time::sleep(wait) => {},
        }
    }
    if let Some((task, worker, _)) = active {
        worker.abort();
        let _ = worker.await;
        log.task(
            "info",
            "task.interrupted",
            &task,
            json!({"reason": "shutdown"}),
        );
    }
    state.recover();
    storage::write_json(&dir.join("state.json"), &state)?;
    listener_task.abort();
    log.event("info", "server.stopped", json!({"reason": "signal"}));
    Ok(())
}

fn finish(state: &mut State, key: &str, result: Result<Outcome, String>, now: i64) {
    let Some(task) = state.tasks.iter_mut().find(|t| t.key == key) else {
        return;
    };
    match result {
        Ok(outcome) => {
            task.last_error = None;
            if let Some(delay) = outcome.repeat_after_ms {
                task.status = Status::Pending;
                task.due_ms = now + delay;
                task.attempts = 0;
            } else {
                task.status = Status::Succeeded;
            }
            for new_task in outcome.tasks {
                state.enqueue(new_task);
            }
        }
        Err(error) => {
            task.last_error = Some(error);
            let delay = (1_000_i64 << task.attempts.min(6)).min(60_000);
            task.due_ms = now + delay;
            if task.expires_ms.is_some_and(|end| task.due_ms >= end) {
                task.status = Status::Expired;
            } else if task
                .max_attempts
                .is_some_and(|limit| task.attempts >= limit)
            {
                task.status = Status::Failed;
            } else {
                task.status = Status::Pending;
            }
        }
    }
}

async fn accept(listener: TcpListener, token: String, tx: mpsc::Sender<Incoming>) {
    let slots = Arc::new(tokio::sync::Semaphore::new(32));
    while let Ok((mut stream, _)) = listener.accept().await {
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let token = token.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let _ = timeout(Duration::from_secs(5), async {
                let envelope: Envelope = protocol::read_message(&mut stream).await?;
                let response = if envelope.token != token {
                    Response::error("Invalid server token")
                } else {
                    let (reply, received) = oneshot::channel();
                    tx.send((envelope.request, reply))
                        .await
                        .map_err(io::Error::other)?;
                    received.await.map_err(io::Error::other)?
                };
                protocol::write_message(&mut stream, &response).await
            })
            .await;
        });
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! { _ = terminate.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
    }
    #[cfg(not(unix))]
    {
        if let Err(error) = tokio::signal::ctrl_c().await {
            eprintln!("Signal handler unavailable: {error}; use `buaa server off`");
            std::future::pending::<()>().await;
        }
    }
}

struct EndpointGuard(PathBuf);
impl Drop for EndpointGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
