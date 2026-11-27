use std::fmt::Write;
use std::path::Path;
use std::time::Duration;

use buaa_api::Context;
use buaa_api::time::DateTime;

use crate::notify;
use crate::server::{self, protocol, storage};

pub(super) fn api_error(error: buaa_api::error::Error) -> String {
    if matches!(error.kind(), buaa_api::error::Kind::Auth) {
        "学校登录需要更新，请在 WSL 终端运行 buaa login。".into()
    } else {
        format!("学校接口请求失败（{}）。", error.kind())
    }
}

pub(super) fn class_records<T>(result: buaa_api::Result<Vec<T>>) -> Result<Vec<T>, String> {
    match result {
        // The pinned API defines Class STATUS 2 as an empty list, but
        // exposes it as a generic server error without a typed code.
        Err(error)
            if error.to_string().lines().next()
                == Some("[ServerInternalError]::[Class]: Empty data list") =>
        {
            Ok(Vec::new())
        }
        other => other.map_err(api_error),
    }
}

pub(super) async fn read(
    operation: &str,
    argument: &str,
    path: &Path,
    disable_tls: bool,
) -> Result<String, String> {
    match operation {
        "status" => return server_status(path).await,
        "notifications" => return notifications(path),
        "selected" | "statistics" | "courses" | "all_courses" | "class" | "exam" | "tes"
        | "tes_all" | "tes_form" => {}
        _ => return Err("不支持的查询。".into()),
    }
    let session = super::session(path, disable_tls)?;
    let context = &session.context;
    let result = match operation {
        "selected" => super::selected(context).await,
        "statistics" => super::statistics(context).await,
        "courses" | "all_courses" => courses(context, argument, operation == "all_courses").await,
        "class" => class(context, argument).await,
        "exam" => exams(context).await,
        "tes" | "tes_all" => evaluations(context, operation == "tes_all").await,
        "tes_form" => evaluation_form(context, argument).await,
        _ => unreachable!(),
    };
    let _ = session.save(false);
    result
}

async fn server_status(path: &Path) -> Result<String, String> {
    let dir = server::directory(path);
    let (running, state) = match protocol::call(&dir, protocol::Request::Status).await {
        Ok(response) => (true, response.state.ok_or("Win 选课服务未返回任务状态。")?),
        Err(_) => {
            let _guard = storage::lock(&dir).map_err(|_| "Win 选课服务未响应。")?;
            let saved = storage::load_state(&dir).map_err(|_| "无法读取当前任务状态。")?;
            (false, saved)
        }
    };
    let process = if running { "运行中" } else { "已停止" };
    let mut text = format!("Win 选课服务{process}（北京时间）");
    let config: crate::boya_auto::Policy =
        serde_json::from_value(state.settings["boya"].clone()).unwrap_or_default();
    let _ = write!(text, "\n自动签到：{}", signed_switch(config.auto_sign));
    for (key, label) in super::CATEGORIES {
        if let Some(target) = config.targets.get(key) {
            let _ = write!(text, " · {label}目标 {target}");
        }
    }
    for task in &state.tasks {
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

fn notifications(path: &Path) -> Result<String, String> {
    let dir = server::directory(path);
    let bark = notify::bark_state(&dir).map_err(|_| "无法读取 Bark 状态。")?;
    let qq = notify::qq_directory(&dir).map_err(|_| "无法读取 QQ 通知状态。")?;
    let status = if let Some(directory) = qq {
        let pending = directory
            .join("outbox")
            .read_dir()
            .into_iter()
            .flatten()
            .flatten()
            .filter(|item| item.path().extension().is_some_and(|ext| ext == "json"))
            .count();
        format!("QQ：已配置，待投递 {pending} 条")
    } else {
        "QQ：未配置".into()
    };
    Ok(format!("{status}\nBark：{bark}"))
}

fn signed_switch(value: bool) -> &'static str {
    if value { "开启" } else { "关闭" }
}

pub(super) fn day(argument: &str) -> Result<DateTime, &'static str> {
    match argument {
        "" | "今天" => Ok(DateTime::now()),
        "明天" => Ok(DateTime::now() + Duration::from_secs(86_400)),
        value => crate::class::parse_datetime(value).map_err(|_| "日期请用 YYYYMMDD。"),
    }
}

async fn class(context: &Context, argument: &str) -> Result<String, String> {
    let class = context.class();
    let signed = |yes| if yes { "已签到" } else { "未签到" };
    let items = match argument {
        "" | "今天" | "明天" => daily_schedule(context, argument).await?,
        id if id.bytes().all(|byte| byte.is_ascii_digit()) => match id.len() {
            8 => daily_schedule(context, argument).await?,
            5 | 6 => class_records(class.query_course_schedule(id).await)?
                .into_iter()
                .map(|s| format!("{} · {} · 排课 {}", s.time.format(), signed(s.status), s.id))
                .collect(),
            9 | 10 => class_records(class.query_course(id).await)?
                .into_iter()
                .map(|c| format!("{} · {} · 课程 {}", c.name, c.teacher, c.id))
                .collect(),
            _ => return Err("请提供 8 位日期、5/6 位课程 ID 或 9/10 位学期 ID。".into()),
        },
        _ => return Err("请提供今天、明天、YYYYMMDD、课程 ID 或学期 ID。".into()),
    };
    Ok(list("智慧教室（北京时间）", items))
}

async fn daily_schedule(context: &Context, argument: &str) -> Result<Vec<String>, String> {
    Ok(
        class_records(context.class().query_schedule(&day(argument)?).await)?
            .into_iter()
            .map(|s| {
                format!(
                    "{} · {} · {}\n排课 {} · 课程 {} · {}",
                    s.time.format(),
                    s.name,
                    s.teacher,
                    s.id,
                    s.course_id,
                    if s.status { "已签到" } else { "未签到" }
                )
            })
            .collect(),
    )
}

async fn exams(context: &Context) -> Result<String, String> {
    let items = crate::exam::query(context)
        .await
        .map_err(|error| match error {
            crate::exam::QueryError::Request(error) => api_error(error),
            crate::exam::QueryError::Data => "学校返回的考试数据无法解析。".into(),
        })?
        .into_iter()
        .map(|e| {
            format!(
                "{} · {}\n{} 至 {}",
                e.name,
                e.position,
                e.start.format(),
                e.end.format()
            )
        })
        .collect();
    Ok(list("考试安排（北京时间）", items))
}

async fn courses(context: &Context, argument: &str, all: bool) -> Result<String, String> {
    let page = if argument.is_empty() {
        1
    } else {
        argument
            .parse::<u8>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or("页码需为 1–255。")?
    };
    let now = DateTime::now();
    let items = context
        .boya()
        .query_courses(page, 10)
        .await
        .map_err(api_error)?
        .into_iter()
        .filter(|c| all || crate::boya::visible_course(c, now))
        .map(|c| {
            format!(
                "{} · {} · ID {}{}\n上课 {} · 选课截止 {}",
                c.name,
                crate::boya::tabled_category(&c.category),
                c.id,
                if c.selected { "（已选）" } else { "" },
                c.schedule.course_start.format(),
                c.schedule.select_end.format()
            )
        })
        .collect();
    Ok(list(&format!("博雅课程第 {page} 页（北京时间）"), items))
}

async fn evaluations(context: &Context, all: bool) -> Result<String, String> {
    let items = evaluation_tasks(context)
        .await?
        .into_iter()
        .enumerate()
        .filter(|(_, task)| all || !task.state)
        .map(|(index, task)| {
            format!(
                "{}. {} · {} · {}",
                index + 1,
                task.course,
                task.teacher,
                if task.state { "已评" } else { "待评" }
            )
        })
        .collect();
    Ok(list("评教任务", items))
}

pub(super) async fn evaluation_tasks(
    context: &Context,
) -> Result<Vec<buaa_api::api::tes::Task>, String> {
    match context.tes().get_task().await {
        // This pinned API reports the confirmed empty task-list response as
        // an error because there is no first task ID to extract.
        Err(error)
            if error.to_string().lines().next()
                == Some("[ServerInternalError]::[Tes]: Empty task") =>
        {
            Ok(Vec::new())
        }
        other => other.map_err(api_error),
    }
}

async fn evaluation_form(context: &Context, argument: &str) -> Result<String, String> {
    let task = super::actions::evaluation_task(context, argument).await?;
    let form = context.tes().get_form(&task).await.map_err(api_error)?;
    let items = form
        .questions
        .iter()
        .enumerate()
        .map(|(index, q)| {
            let options = if q.is_choice {
                q.choices
                    .iter()
                    .enumerate()
                    .map(|(i, choice)| format!("{}={}", i + 1, choice.score))
                    .collect::<Vec<_>>()
                    .join(" / ")
            } else {
                "开放题".into()
            };
            format!("{}. {}\n{}", index + 1, q.name, options)
        })
        .collect();
    Ok(list(
        &format!("{} · {} 的问卷", task.course, task.teacher),
        items,
    ))
}

pub(super) fn list(heading: &str, items: Vec<String>) -> String {
    if items.is_empty() {
        format!("{heading}\n暂无记录。")
    } else {
        format!("{heading}\n{}", items.join("\n"))
    }
}
