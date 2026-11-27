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
    code: String,
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
    if request.operation == "confirm" {
        let action = take_confirmation(pending, &request.argument)?;
        return timeout(Duration::from_secs(90), action.execute(path, disable_tls))
            .await
            .unwrap_or_else(|_| Err("操作超时，执行结果未确认。请先查询实际状态。".into()));
    }
    if let Some(operation) = request.operation.strip_prefix("prepare.") {
        let (preview, action) = timeout(
            Duration::from_secs(20),
            actions::Action::prepare(operation, &request.argument, path, disable_tls),
        )
        .await
        .map_err(|_| "准备操作超时，请稍后重试。")??;
        let mut random = [0; 4];
        getrandom::fill(&mut random).map_err(|_| "无法生成确认码。")?;
        let code = format!("{:08X}", u32::from_be_bytes(random));
        *pending = Some(Pending {
            code: code.clone(),
            expires: Instant::now() + Duration::from_secs(120),
            action,
        });
        return Ok(format!(
            "{preview}\n输入 /北航 确认 {code} 执行。确认码 2 分钟有效。"
        ));
    }
    timeout(
        Duration::from_secs(15),
        queries::read(&request.operation, &request.argument, path, disable_tls),
    )
    .await
    .unwrap_or_else(|_| Err("查询超时，请稍后重试。".into()))
}

fn take_confirmation(
    pending: &mut Option<Pending>,
    code: &str,
) -> Result<actions::Action, &'static str> {
    if pending
        .as_ref()
        .is_some_and(|p| p.expires <= Instant::now())
    {
        *pending = None;
    }
    let entry = pending
        .as_ref()
        .ok_or("没有待确认的操作，或确认码已失效。")?;
    if !entry.code.eq_ignore_ascii_case(code) {
        return Err("确认码不匹配。");
    }
    Ok(pending.take().expect("checked pending entry").action)
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

    #[test]
    fn confirmation_expires_and_cannot_be_replayed() {
        let mut pending = Some(Pending {
            code: "A10B".into(),
            expires: Instant::now() + Duration::from_secs(2),
            action: actions::Action::Bark(false),
        });
        assert!(take_confirmation(&mut pending, "AAAA").is_err());
        assert!(matches!(
            take_confirmation(&mut pending, "a10b"),
            Ok(actions::Action::Bark(false))
        ));
        assert!(take_confirmation(&mut pending, "A10B").is_err());
        pending = Some(Pending {
            code: "A10B".into(),
            expires: Instant::now(),
            action: actions::Action::Bark(false),
        });
        assert!(take_confirmation(&mut pending, "A10B").is_err());
        assert!(pending.is_none());
    }
}
