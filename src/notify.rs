//! Bark configuration and delivery. The Device Key stays out of task state and IPC.

use crate::command::{BarkSub, NotifySub};
use crate::server::{
    self, Handler,
    logging::Log,
    storage,
    task::{State, Task},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::{sync::mpsc, task::JoinHandle};

const HOST: &str = "https://api.day.app";
const ENDPOINT: &str = "https://api.day.app/push";
const CONFIG_FILE: &str = "notify.json";

#[derive(Deserialize, Serialize)]
struct Config {
    device_key: String,
    enabled: bool,
}

#[derive(Deserialize)]
struct QqConfig {
    directory: PathBuf,
}

pub(crate) fn qq_directory(dir: &Path) -> io::Result<Option<PathBuf>> {
    match storage::read_json::<QqConfig>(&dir.join("qq.json")) {
        Ok(config) if config.directory.is_absolute() => Ok(Some(config.directory)),
        Ok(_) => Err(io::Error::other("QQ directory must be absolute")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(io::Error::other("Cannot read QQ configuration")),
    }
}

#[derive(Serialize)]
pub(crate) struct Message {
    pub title: String,
    pub body: String,
    pub group: String,
}

fn load(dir: &Path) -> io::Result<Option<Config>> {
    match storage::read_json(&dir.join(CONFIG_FILE)) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(io::Error::other("Cannot read Bark configuration")),
    }
}

fn update(dir: &Path, change: impl FnOnce(Option<Config>) -> io::Result<Config>) -> io::Result<()> {
    storage::private_dir(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("notify.lock"))?;
    lock.lock()?;
    let config = change(load(dir)?)?;
    storage::write_json(&dir.join(CONFIG_FILE), &config)
}

fn key(input: &str) -> io::Result<&str> {
    let input = input.trim();
    let input = input
        .strip_prefix("https://api.day.app/")
        .map_or(input, |path| path.trim_end_matches('/'));
    if input.is_empty()
        || input.len() > 256
        || !input
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(io::Error::other(
            "Enter a Bark Device Key or the official Bark device URL",
        ));
    }
    Ok(input)
}

fn require_config(config: Option<Config>) -> io::Result<Config> {
    config.ok_or_else(|| io::Error::other("Run `buaa notify bark set` first"))
}

#[cfg(unix)]
pub(crate) fn bark_state(dir: &Path) -> io::Result<&'static str> {
    Ok(match load(dir)? {
        Some(config) if config.enabled => "开启",
        Some(_) => "已暂停",
        None => "未配置",
    })
}

pub(crate) fn set_bark(dir: &Path, enabled: bool) -> io::Result<()> {
    update(dir, |config| {
        let mut config = require_config(config)?;
        config.enabled = enabled;
        Ok(config)
    })
}

pub(crate) async fn test_bark(dir: &Path) -> io::Result<()> {
    let config = require_config(load(dir)?)?;
    send(
        &client()?,
        ENDPOINT,
        &config,
        &Message {
            title: "BUAA CLI · Bark 测试通知".into(),
            body: "这是一条测试消息。收到它表示 BUAA CLI 可以向你的设备推送通知。".into(),
            group: "BUAA CLI".into(),
        },
    )
    .await
}

#[cfg(unix)]
pub(crate) fn test_qq(dir: &Path) -> io::Result<()> {
    let directory = qq_directory(dir)?.ok_or_else(|| io::Error::other("QQ 未配置。"))?;
    let outbox = directory.join("outbox");
    storage::private_dir(&outbox)?;
    enqueue_qq(
        &outbox,
        server::task::now_ms(),
        &Message {
            title: "BUAA CLI · QQ 测试通知".into(),
            body: "这是你在 QQ 私聊中确认发送的测试消息。".into(),
            group: "BUAA CLI".into(),
        },
    )
}

pub async fn command(data_dir: &Path, command: &NotifySub) -> io::Result<()> {
    let dir = server::directory(data_dir);
    match command {
        NotifySub::Bark {
            command: BarkSub::Set,
        } => {
            let input = if io::stdin().is_terminal() {
                rpassword::prompt_password("Bark Device Key: ")?
            } else {
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                input
            };
            let device_key = key(&input)?.to_owned();
            update(&dir, |_| {
                Ok(Config {
                    device_key,
                    enabled: true,
                })
            })?;
            println!(
                "Bark notifications enabled ({HOST}); run `buaa notify test` to check delivery."
            );
        }
        NotifySub::Status => match load(&dir)? {
            Some(config) => println!(
                "Bark: {} ({HOST}); Device Key configured",
                if config.enabled { "enabled" } else { "paused" },
            ),
            None => println!("Bark: not configured ({HOST})"),
        },
        NotifySub::On | NotifySub::Off => {
            let enabled = matches!(command, NotifySub::On);
            set_bark(&dir, enabled)?;
            println!(
                "Bark notifications {}.",
                if enabled { "enabled" } else { "paused" }
            );
        }
        NotifySub::Test => {
            test_bark(&dir).await?;
            println!("Bark accepted the test notification; check your phone.");
        }
    }
    Ok(())
}

fn client() -> io::Result<reqwest::Client> {
    reqwest::Client::builder()
        .https_only(true)
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|_| io::Error::other("Cannot initialize Bark HTTP client"))
}

pub(crate) fn within_bytes(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

async fn send(
    client: &reqwest::Client,
    endpoint: &str,
    config: &Config,
    message: &Message,
) -> io::Result<()> {
    #[derive(Deserialize)]
    struct Ack {
        code: u16,
    }

    let response = client
        .post(endpoint)
        .json(&serde_json::json!({
            "device_key": config.device_key,
            "title": within_bytes(&message.title, 240),
            "body": within_bytes(&message.body, 2400),
            "group": message.group
        }))
        .send()
        .await
        .map_err(|error| {
            io::Error::other(if error.is_timeout() {
                "Bark request timed out"
            } else {
                "Bark connection failed"
            })
        })?;
    if !response.status().is_success() {
        return Err(io::Error::other(format!(
            "Bark returned HTTP {}",
            response.status().as_u16()
        )));
    }
    let ack: Ack = response
        .json()
        .await
        .map_err(|_| io::Error::other("Invalid Bark confirmation"))?;
    if ack.code != 200 {
        return Err(io::Error::other(format!(
            "Bark rejected the notification (code {})",
            ack.code
        )));
    }
    Ok(())
}

struct Event {
    task: Task,
    state: State,
    finished_ms: i64,
}

pub(crate) struct Queue {
    tx: mpsc::Sender<Event>,
    worker: JoinHandle<()>,
    log: Log,
}

impl Queue {
    pub fn start<H: Handler>(dir: PathBuf, handler: Arc<H>, log: Log) -> Self {
        let (tx, rx) = mpsc::channel(32);
        let worker = tokio::spawn(deliver(dir, handler, log.clone(), rx));
        Self { tx, worker, log }
    }

    pub fn submit(&self, task: Task, state: &State, finished_ms: i64) {
        if let Ok(permit) = self.tx.try_reserve() {
            permit.send(Event {
                task,
                state: state.clone(),
                finished_ms,
            });
        } else {
            self.log.task(
                "warn",
                "notification.dropped",
                &task,
                json!({
                    "reason": "notification queue unavailable"
                }),
            );
        }
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

async fn deliver<H: Handler>(
    dir: PathBuf,
    handler: Arc<H>,
    log: Log,
    mut rx: mpsc::Receiver<Event>,
) {
    let qq = match qq_directory(&dir).and_then(|directory| {
        directory
            .map(|directory| {
                storage::private_dir(&directory)?;
                let outbox = directory.join("outbox");
                storage::private_dir(&outbox)?;
                Ok::<_, io::Error>(outbox)
            })
            .transpose()
    }) {
        Ok(directory) => directory,
        Err(_) => {
            log.event(
                "warn",
                "notification.failed",
                json!({
                    "provider": "qq", "error": "Cannot initialize QQ outbox"
                }),
            );
            None
        }
    };
    let mut http = None;
    while let Some(event) = rx.recv().await {
        let result = async {
            if qq.is_none() && !load(&dir)?.is_some_and(|config| config.enabled) {
                return Ok(());
            }
            let Ok(message) = tokio::time::timeout(
                Duration::from_secs(4),
                handler.notification(&event.task, &event.state, event.finished_ms),
            )
            .await
            else {
                return Err(io::Error::other("Notification formatting timed out"));
            };
            let Some(message) = message else {
                return Ok(());
            };
            if let Some(outbox) = &qq {
                let record = enqueue_qq(outbox, event.finished_ms, &message);
                log.task(
                    if record.is_ok() { "info" } else { "warn" },
                    if record.is_ok() {
                        "notification.queued"
                    } else {
                        "notification.failed"
                    },
                    &event.task,
                    if record.is_ok() {
                        json!({"provider": "qq"})
                    } else {
                        json!({"provider": "qq", "error": "Cannot write QQ notification"})
                    },
                );
            }
            // Honor an off command or key rotation while the text was prepared.
            let Some(config) = load(&dir)?.filter(|config| config.enabled) else {
                return Ok(());
            };
            if http.is_none() {
                http = Some(client()?);
            }
            send(http.as_ref().unwrap(), ENDPOINT, &config, &message).await?;
            log.task(
                "info",
                "notification.sent",
                &event.task,
                json!({"provider": "bark"}),
            );
            Ok::<(), io::Error>(())
        }
        .await;
        if let Err(error) = result {
            log.task(
                "warn",
                "notification.failed",
                &event.task,
                json!({
                    "provider": "bark", "error": error.to_string()
                }),
            );
        }
    }
}

fn enqueue_qq(outbox: &Path, finished_ms: i64, message: &Message) -> io::Result<()> {
    let mut nonce = [0; 8];
    getrandom::fill(&mut nonce).map_err(|_| io::Error::other("Cannot identify QQ event"))?;
    let name = format!("{finished_ms:013}-{:016x}.json", u64::from_be_bytes(nonce));
    storage::write_json(&outbox.join(name), message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn bark_requires_an_application_ack_and_does_not_expose_response_text() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/push", listener.local_addr().unwrap());
        let peer = tokio::spawn(async move {
            for body in [
                r#"{"code":200}"#,
                r#"{"code":400,"message":"example-device-key"}"#,
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                // The real request has a small, length-delimited body.
                let mut reader = tokio::io::BufReader::new(&mut socket);
                let mut headers = String::new();
                loop {
                    use tokio::io::AsyncBufReadExt;
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    assert!(!line.is_empty());
                    headers.push_str(&line);
                }
                assert!(headers.starts_with("POST /push "));
                assert!(!headers.contains("example-device-key"));
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).await.unwrap();
                let message: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(message["device_key"], "example-device-key");
                assert_eq!(message["group"], "博雅");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .no_proxy()
            .build()
            .unwrap();
        let config = Config {
            device_key: "example-device-key".into(),
            enabled: true,
        };
        let message = Message {
            title: "标题".into(),
            body: "正文".into(),
            group: "博雅".into(),
        };
        send(&client, &endpoint, &config, &message).await.unwrap();
        let error = send(&client, &endpoint, &config, &message)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("code 400"));
        assert!(!error.contains(&config.device_key));
        peer.await.unwrap();
    }
}
