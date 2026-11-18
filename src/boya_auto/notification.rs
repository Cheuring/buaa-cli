use super::{
    Backend, BoyaHandler, Course, CourseTask, Policy, State, Task, category_key, counts, policy,
};
use crate::notify::Message;
use crate::server::task::Status;
use buaa_api::time::DateTime;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::time::Duration;

#[derive(Deserialize)]
struct CourseSummary {
    name: String,
    category: String,
    location: String,
    window_start_ms: i64,
}

pub(super) fn course_summary(course: &Course, window_start_ms: i64) -> Value {
    json!({
        "name": one_line(&course.name, 100),
        "category": category_key(course.category),
        "location": one_line(&course.location, 100),
        "window_start_ms": window_start_ms
    })
}

fn summary(task: &Task) -> Option<CourseSummary> {
    serde_json::from_value(task.notification_data.clone()?).ok()
}

fn one_line(text: &str, max_chars: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

fn category(key: &str) -> &str {
    match key {
        "ethics" => "德育",
        "arts" => "美育",
        "labor" => "劳动教育",
        "safety" => "安全健康",
        "other" => "其他",
        _ => "博雅",
    }
}

fn action(task: &Task, payload: &CourseTask) -> Option<&'static str> {
    match task.kind.as_str() {
        "boya.select" => Some("选课"),
        "boya.sign" if payload.checkout => Some("签退"),
        "boya.sign" => Some("签到"),
        _ => None,
    }
}

fn course_name(summary: Option<&CourseSummary>, id: u32) -> String {
    summary
        .filter(|course| !course.name.is_empty())
        .map_or_else(|| format!("课程 {id}"), |course| course.name.clone())
}

fn time(ms: i64) -> String {
    DateTime::from_timestamp(ms / 1_000).format()
}

pub(super) async fn render(
    handler: &BoyaHandler,
    task: &Task,
    state: &State,
    finished_ms: i64,
) -> Option<Message> {
    let payload = serde_json::from_value::<CourseTask>(task.payload.clone()).ok()?;
    let action = action(task, &payload)?;
    let config = policy(&state.settings).unwrap_or_default();
    let selected = if config.targets.is_empty() {
        None
    } else {
        // Statistics are optional and must not delay a notification indefinitely.
        tokio::time::timeout(Duration::from_secs(2), async {
            let session =
                crate::auth::Session::load(&handler.data_dir, handler.disable_tls, false).ok()?;
            session
                .context
                .selected()
                .await
                .ok()
                .map(|courses| counts(&courses))
        })
        .await
        .ok()
        .flatten()
    };
    Some(format(
        task,
        state,
        finished_ms,
        &payload,
        action,
        &config,
        selected.as_ref(),
    ))
}

fn format(
    task: &Task,
    state: &State,
    finished_ms: i64,
    payload: &CourseTask,
    action: &str,
    config: &Policy,
    selected: Option<&BTreeMap<String, u32>>,
) -> Message {
    let success = task.status == Status::Succeeded;
    let course = summary(task);
    let label = course
        .as_ref()
        .map_or("博雅", |course| category(&course.category));
    let result = if success { "成功" } else { "失败" };
    let name = course_name(course.as_ref(), payload.id);
    let title = format!(
        "[{label}]《{}》{action}{result}",
        crate::notify::within_bytes(&name, 180)
    );
    let mut body = String::new();
    if !success {
        let reason = task.last_error.as_deref().unwrap_or("未能在有效窗口内完成");
        let _ = writeln!(body, "失败原因：{}", one_line(reason, 180));
    }
    let time_label = if success {
        "完成时间"
    } else {
        "结束时间"
    };
    let _ = writeln!(body, "{time_label}：{}（北京时间）", time(finished_ms));
    let _ = writeln!(body, "课程 ID：{}", payload.id);
    if let Some(course) = &course {
        if !course.location.is_empty() {
            let _ = writeln!(body, "开课地点：{}", course.location);
        }
        if let Some(end) = task.expires_ms {
            let _ = writeln!(
                body,
                "{action}计划窗口：{} 至 {}",
                time(course.window_start_ms),
                time(end)
            );
        }
    }
    if !config.targets.is_empty() {
        body.push_str("\n本学期已选 / 目标：\n");
        for key in ["ethics", "arts", "labor", "safety", "other"] {
            if let Some(target) = config.targets.get(key) {
                let total = selected.map_or("未知".into(), |counts| {
                    counts.get(key).copied().unwrap_or(0).to_string()
                });
                let _ = writeln!(body, "- {}：{total} / {target}", category(key));
            }
        }
    }
    let mut planned: Vec<_> = state
        .tasks
        .iter()
        .filter(|pending| pending.status == Status::Pending)
        .filter_map(|pending| {
            let payload = serde_json::from_value::<CourseTask>(pending.payload.clone()).ok()?;
            let action = self::action(pending, &payload)?;
            Some((
                pending.due_ms,
                action,
                course_name(summary(pending).as_ref(), payload.id),
            ))
        })
        .collect();
    planned.sort_by_key(|(due, _, _)| *due);
    if !planned.is_empty() {
        body.push_str("\n接下来已入队：\n");
        for (due, action, name) in planned.into_iter().take(4) {
            let _ = writeln!(body, "- [{action}] {} · {}", time(due), one_line(&name, 48));
        }
    }
    Message {
        title,
        body,
        group: "博雅".into(),
    }
}
