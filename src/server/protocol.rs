use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{Duration, timeout};

use super::{storage, task::State};

const MAX_MESSAGE: u64 = 1024 * 1024;

#[derive(Deserialize, Serialize)]
pub struct Endpoint {
    pub port: u16,
    pub pid: u32,
    pub token: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    Status,
    Stop,
    Configure { namespace: String, payload: Value },
    Submit { kind: String, payload: Value },
}

#[derive(Deserialize, Serialize)]
pub struct Envelope {
    pub token: String,
    pub request: Request,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Response {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<State>,
}

impl Response {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            state: None,
        }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            state: None,
        }
    }
}

pub async fn read_message<T: for<'de> Deserialize<'de>>(stream: &mut TcpStream) -> io::Result<T> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    let size = BufReader::new(stream)
        .take(MAX_MESSAGE + 1)
        .read_until(b'\n', &mut bytes)
        .await?;
    if size == 0 || size as u64 > MAX_MESSAGE || bytes.last() != Some(&b'\n') {
        return Err(io::Error::other("Invalid or oversized IPC message"));
    }
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

pub async fn write_message<T: Serialize>(stream: &mut TcpStream, value: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await
}

pub async fn call(dir: &Path, request: Request) -> io::Result<Response> {
    let endpoint: Endpoint = storage::read_json(&dir.join("endpoint.json")).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("Server unavailable; run `buaa server start`: {error}"),
        )
    })?;
    timeout(Duration::from_secs(5), async {
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, endpoint.port)).await?;
        write_message(
            &mut stream,
            &Envelope {
                token: endpoint.token,
                request,
            },
        )
        .await?;
        read_message(&mut stream).await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Server request timed out"))?
}
