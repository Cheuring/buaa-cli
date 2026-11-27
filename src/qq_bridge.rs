//! Private, read-only queries for the AstrBot plugin on the same Linux host.

use std::fmt::Write;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use buaa_api::api::boya::Assessment;
use buaa_api::error::Kind;
use buaa_api::time::DateTime;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::auth::Session;
use crate::boya_auto::backend::Backend;
use crate::server::{self, protocol, storage};

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

pub(crate) fn start(data_dir: &Path, disable_tls: bool) -> io::Result<Option<Bridge>> {
    let Some(directory) = crate::notify::qq_directory(&server::directory(data_dir))? else {
        return Ok(None);
    };
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
        while let Ok((stream, _)) = listener.accept().await {
            let _ = serve(stream, &data_dir, disable_tls).await;
        }
    });
    Ok(Some(Bridge {
        worker,
        socket,
        _lock: lock,
    }))
}

async fn serve(stream: UnixStream, data_dir: &Path, disable_tls: bool) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut command = String::new();
    timeout(
        Duration::from_secs(2),
        BufReader::new(reader).take(32).read_line(&mut command),
    )
    .await??;
    let result = timeout(
        Duration::from_secs(12),
        query(
            command.strip_suffix('\n').unwrap_or(""),
            data_dir,
            disable_tls,
        ),
    )
    .await
    .unwrap_or_else(|_| Err("博雅查询超时，请稍后重试。".into()));
    let mut reply = serde_json::to_vec(&match result {
        Ok(text) => json!({"ok": true, "text": text}),
        Err(text) => json!({"ok": false, "text": text}),
    })?;
    reply.push(b'\n');
    timeout(Duration::from_secs(2), writer.write_all(&reply)).await?
}

async fn query(command: &str, path: &Path, disable_tls: bool) -> Result<String, String> {
    if command == "status" {
        return server_status(path).await;
    }
    if !matches!(command, "selected" | "statistics") {
        return Err("不支持的查询。".into());
    }
    let session = Session::load(path, disable_tls, false)
        .map_err(|_| "学校登录状态不可用，请在 WSL 里运行 buaa login。")?;
    let text = if command == "selected" {
        let courses = session.context.selected().await?;
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
        text
    } else {
        let result = session
            .context
            .boya()
            .query_statistic()
            .await
            .map_err(|error| {
                if matches!(error.kind(), Kind::Auth) {
                    "学校登录需要更新，请在 WSL 里运行 buaa login。".into()
                } else {
                    format!("博雅统计查询失败（{}）。", error.kind())
                }
            })?;
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
        text
    };
    let _ = session.save(false);
    Ok(text)
}

async fn server_status(path: &Path) -> Result<String, String> {
    let response = protocol::call(&server::directory(path), protocol::Request::Status)
        .await
        .map_err(|_| "无法连接 Win 选课服务。")?;
    let state = response.state.ok_or("Win 选课服务未返回任务状态。")?;
    let mut text = String::from("Win 选课服务运行中（时间为北京时间）");
    if let Some(config) = state.settings.get("boya") {
        let targets: crate::boya_auto::Policy =
            serde_json::from_value(config.clone()).unwrap_or_default();
        let _ = write!(
            text,
            "\n自动签到：{}",
            if targets.auto_sign { "开" } else { "关" }
        );
        for (key, label) in [
            ("ethics", "德育"),
            ("arts", "美育"),
            ("labor", "劳动教育"),
            ("safety", "安全健康"),
            ("other", "其他"),
        ] {
            if let Some(target) = targets.targets.get(key) {
                let _ = write!(text, " · {label}目标 {target}");
            }
        }
    }
    for task in state.tasks {
        let _ = write!(
            text,
            "\n{} · {:?} · {}",
            task.key,
            task.status,
            DateTime::from_timestamp(task.due_ms / 1_000).format(),
        );
    }
    Ok(text)
}
