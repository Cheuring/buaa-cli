//! Local daemon infrastructure. Domain behavior is supplied by a Handler.

pub mod protocol;
pub mod storage;
pub mod task;
#[cfg(test)]
mod tests;

use protocol::{Endpoint, Envelope, Request, Response};
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use task::{Outcome, State, Status, Task, now_ms};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

pub trait Handler: Send + Sync + 'static {
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
    let response = protocol::call(&directory(data_dir), request).await?;
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
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("server.log"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .env("BUAA_DATA_DIR", data_dir)
        .args(["server", "run"])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
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
    let mut child = command.spawn()?;
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
                dir.join("server.log").display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Do not leave an unreported child after a failed startup handshake.
    let _ = child.kill();
    let _ = child.wait();
    Err(io::Error::other(
        "Server startup timed out; inspect server.log",
    ))
}

type Incoming = (Request, oneshot::Sender<Response>);
type Worker = JoinHandle<Result<Outcome, String>>;

pub async fn run<H: Handler>(data_dir: &Path, handler: H) -> io::Result<()> {
    let dir = directory(data_dir);
    storage::private_dir(&dir)?;
    let _lock = storage::lock(&dir)?;
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
    println!("[{}] Server started (PID {})", now_ms(), endpoint.pid);
    let (tx, mut rx) = mpsc::channel::<Incoming>(32);
    let listener_task = tokio::spawn(accept(listener, endpoint.token, tx));
    let handler = Arc::new(handler);
    let mut active: Option<(Task, Worker)> = None;
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        if state.expire(now_ms()) {
            storage::write_json(&dir.join("state.json"), &state)?;
        }
        if let Some((task, _)) = &active
            && task.preemptible
            && state
                .next_due(now_ms())
                .is_some_and(|index| state.tasks[index].priority > task.priority)
        {
            let (task, worker) = active.take().unwrap();
            worker.abort();
            let _ = worker.await;
            if let Some(interrupted) = state.tasks.iter_mut().find(|t| t.key == task.key) {
                interrupted.status = Status::Pending;
                interrupted.due_ms = now_ms() + 1_000;
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
            let worker = tokio::spawn(async move {
                timeout(Duration::from_secs(45), executor.execute(work, settings))
                    .await
                    .unwrap_or_else(|_| Err("Task timed out; will reconcile before retry".into()))
            });
            active = Some((task, worker));
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
                    if let Some((task, _)) = &active
                        && !state.tasks.iter().any(|t| t.key == task.key && t.status == Status::Running) {
                        let (_, worker) = active.take().unwrap();
                        worker.abort();
                        let _ = worker.await;
                    }
                }
                if stop {
                    // Await current cancellation and release endpoint/lock before
                    // acknowledging, so a following `start` cannot see a ghost.
                    if let Some((_, worker)) = active.take() { worker.abort(); let _ = worker.await; }
                    state.recover();
                    storage::write_json(&dir.join("state.json"), &state)?;
                    listener_task.abort();
                    drop(_endpoint_guard);
                    drop(_lock);
                    let _ = reply.send(response);
                    println!("[{}] Server stopped", now_ms());
                    return Ok(());
                }
                let _ = reply.send(response);
            },
            result = async { (&mut active.as_mut().unwrap().1).await }, if active.is_some() => {
                let (task, _) = active.take().unwrap();
                let result = result.unwrap_or_else(|error| Err(format!("Task worker failed: {error}")));
                finish(&mut state, &task.key, result, now_ms());
                storage::write_json(&dir.join("state.json"), &state)?;
            },
            _ = tokio::time::sleep(wait) => {},
        }
    }
    if let Some((_, worker)) = active {
        worker.abort();
        let _ = worker.await;
    }
    state.recover();
    storage::write_json(&dir.join("state.json"), &state)?;
    listener_task.abort();
    println!("[{}] Server stopped", now_ms());
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
            eprintln!("[{now}] {key}: {error}");
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
        let _ = tokio::signal::ctrl_c().await;
    }
}

struct EndpointGuard(PathBuf);
impl Drop for EndpointGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
