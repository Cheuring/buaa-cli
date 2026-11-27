//! Private BUAA operations for the AstrBot plugin on the same Linux host.

mod actions;
mod queries;

use std::fmt::Write;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use buaa_api::{Context, api::boya::Assessment};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};

use crate::auth::Session;
use crate::boya_auto::backend::Backend;
use crate::server::{self, storage};

const CATEGORIES: [(&str, &str); 5] = [
    ("ethics", "德育"),
    ("arts", "美育"),
    ("labor", "劳动教育"),
    ("safety", "安全健康"),
    ("other", "其他"),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: String,
    #[serde(default)]
    argument: String,
}

struct Pending {
    expires: Instant,
    action: actions::Action,
}

pub(crate) struct Bridge {
    worker: JoinHandle<()>,
    socket: PathBuf,
    _lock: File,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.worker.abort();
        let _ = fs::remove_file(&self.socket);
    }
}

pub(crate) async fn run(data_dir: &Path, disable_tls: bool) -> io::Result<()> {
    let directory = crate::notify::qq_directory(&server::directory(data_dir))?
        .ok_or_else(|| io::Error::other("QQ integration is not configured"))?;
    storage::private_dir(&directory)?;
    let lock = storage::lock(&directory)?;
    let socket = directory.join("query.sock");
    match fs::remove_file(&socket) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let data_dir = data_dir.to_owned();
    let worker = tokio::spawn(async move {
        let mut pending = None;
        while let Ok((stream, _)) = listener.accept().await {
            let _ = serve(stream, &data_dir, disable_tls, &mut pending).await;
        }
    });
    let mut bridge = Bridge {
        worker,
        socket,
        _lock: lock,
    };
    tokio::select! {
        result = &mut bridge.worker => { result?; }
        _ = server::shutdown_signal() => {}
    }
    Ok(())
}

async fn serve(
    stream: UnixStream,
    data_dir: &Path,
    disable_tls: bool,
    pending: &mut Option<Pending>,
) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut command = String::new();
    timeout(
        Duration::from_secs(2),
        BufReader::new(reader).take(4096).read_line(&mut command),
    )
    .await??;
    let result = handle(
        command.strip_suffix('\n').unwrap_or(""),
        data_dir,
        disable_tls,
        pending,
    )
    .await;
    let mut reply = serde_json::to_vec(&match result {
        Ok(text) => json!({"ok": true, "text": text}),
        Err(text) => json!({"ok": false, "text": text}),
    })?;
    reply.push(b'\n');
    timeout(Duration::from_secs(2), writer.write_all(&reply)).await?
}

async fn handle(
    line: &str,
    path: &Path,
    disable_tls: bool,
    pending: &mut Option<Pending>,
) -> Result<String, String> {
    // Keep the original read-only requests usable during a plugin upgrade.
    let request = if matches!(line, "status" | "selected" | "statistics") {
        Request {
            operation: line.into(),
            argument: String::new(),
        }
    } else {
        serde_json::from_str(line).map_err(|_| "不支持的请求。")?
    };
    expire_pending(pending);
    if matches!(request.operation.as_str(), "confirm" | "cancel") {
        if !request.argument.is_empty() {
            return Err("请直接回复「确认」或「取消」。".into());
        }
        let entry = pending.take().ok_or("没有待确认的操作，或操作已过期。")?;
        if request.operation == "cancel" {
            return Ok("已取消这次操作。".into());
        }
        return timeout(
            Duration::from_secs(90),
            entry.action.execute(path, disable_tls),
        )
        .await
        .unwrap_or_else(|_| Err("操作超时，执行结果未确认。请先查询实际状态。".into()));
    }
    if let Some(operation) = request.operation.strip_prefix("prepare.") {
        if pending.is_some() {
            return Err(
                "已有一项待确认的操作。请回复「确认」执行上一项，或回复「取消」后重新发起。".into(),
            );
        }
        let (preview, action) = timeout(
            Duration::from_secs(20),
            actions::Action::prepare(operation, &request.argument, path, disable_tls),
        )
        .await
        .map_err(|_| "准备操作超时，请稍后重试。")??;
        *pending = Some(Pending {
            expires: Instant::now() + Duration::from_secs(120),
            action,
        });
        return Ok(format!(
            "{preview}\n请在 2 分钟内直接回复「确认」执行，或回复「取消」。"
        ));
    }
    timeout(
        Duration::from_secs(15),
        queries::read(&request.operation, &request.argument, path, disable_tls),
    )
    .await
    .unwrap_or_else(|_| Err("查询超时，请稍后重试。".into()))
}

fn expire_pending(pending: &mut Option<Pending>) {
    if pending
        .as_ref()
        .is_some_and(|p| p.expires <= Instant::now())
    {
        *pending = None;
    }
}

fn session(path: &Path, disable_tls: bool) -> Result<Session, &'static str> {
    Session::load(path, disable_tls, false)
        .map_err(|_| "学校登录状态不可用，请在 WSL 终端运行 buaa login。")
}

async fn selected(context: &Context) -> Result<String, String> {
    let courses = context.selected().await?;
    let mut text = format!("本学期已选 {} 门博雅课", courses.len());
    for course in courses {
        let category = crate::boya::tabled_category(&course.category);
        let _ = write!(
            text,
            "\n{} · {} [{}]\n{} · {}",
            course.name,
            category,
            course.id,
            course.schedule.course_start.format(),
            course.location,
        );
    }
    Ok(text)
}

async fn statistics(context: &Context) -> Result<String, String> {
    let result = context
        .boya()
        .query_statistic()
        .await
        .map_err(queries::api_error)?;
    let mut text = String::from("博雅完成进度（门）");
    for (
        name,
        Assessment {
            require,
            complete,
            select,
            ..
        },
    ) in [
        ("德育", result.ethics),
        ("美育", result.arts),
        ("劳动教育", result.labor),
        ("安全健康", result.safety),
    ] {
        let remaining = require.saturating_sub(complete);
        let _ = write!(
            text,
            "\n{name} · 完成 {complete}/{require}，还差 {remaining}，系统显示已选 {select}"
        );
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn confirming_or_cancelling_always_refers_to_the_same_unexpired_preview() {
        let mut pending = None;
        let path = Path::new("");
        let prepare = r#"{"operation":"prepare.bark_off"}"#;
        let confirm = r#"{"operation":"confirm"}"#;
        let cancel = r#"{"operation":"cancel"}"#;
        assert!(handle(confirm, path, false, &mut pending).await.is_err());
        let preview = handle(prepare, path, false, &mut pending).await.unwrap();
        assert!(preview.contains("直接回复「确认」"));
        assert!(
            handle(
                r#"{"operation":"prepare.bark_on"}"#,
                path,
                false,
                &mut pending
            )
            .await
            .is_err()
        );
        assert!(matches!(
            &pending.as_ref().unwrap().action,
            actions::Action::Bark(false)
        ));
        assert!(
            handle(
                r#"{"operation":"confirm","argument":"1234"}"#,
                path,
                false,
                &mut pending
            )
            .await
            .is_err()
        );
        assert!(handle(cancel, path, false, &mut pending).await.is_ok());
        assert!(handle(confirm, path, false, &mut pending).await.is_err());
        handle(prepare, path, false, &mut pending).await.unwrap();
        pending.as_mut().unwrap().expires = Instant::now();
        assert!(handle(confirm, path, false, &mut pending).await.is_err());
        assert!(pending.is_none());
    }
}
