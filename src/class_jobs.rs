//! Durable Smart Classroom check-ins submitted by the authenticated QQ bridge.

use buaa_api::{Context, time::DateTime};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::notify::Message;
use crate::server::task::{Outcome, State, Status, Task, now_ms};

pub(crate) const CHECKIN: &str = "class.checkin";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Checkin {
    pub schedule: String,
    pub due_ms: i64,
}

pub(crate) fn submit(state: &mut State, payload: Value) -> Result<String, String> {
    let jobs: Vec<Checkin> = serde_json::from_value(payload).map_err(|_| "签到任务格式无效。")?;
    if jobs.is_empty() || jobs.len() > 32 || jobs.iter().any(|job| !valid_schedule(&job.schedule)) {
        return Err("签到任务需要 1–32 个有效排课 ID。".into());
    }
    let count = jobs.len();
    for job in jobs {
        let mut task = Task::new(
            format!("{CHECKIN}:{}", job.schedule),
            CHECKIN,
            json!(job.schedule),
            job.due_ms.max(now_ms()),
        );
        task.max_attempts = Some(1);
        task.priority = 20;
        state.enqueue(task);
    }
    Ok(format!("已提交 {count} 个智慧教室签到任务。"))
}

pub(crate) fn valid_schedule(id: &str) -> bool {
    id.len() == 7 && id.bytes().all(|byte| byte.is_ascii_digit())
}

pub(crate) async fn execute(context: &Context, task: &Task) -> Result<Outcome, String> {
    let id = task
        .payload
        .as_str()
        .filter(|id| valid_schedule(id))
        .ok_or("排课 ID 无效。")?;
    context
        .class()
        .checkin(id)
        .await
        .map_err(|error| format!("class.checkin: {}", error.kind()))?;
    Ok(Outcome {
        notify_on_success: true,
        ..Outcome::default()
    })
}

pub(crate) fn notification(task: &Task) -> Option<Message> {
    let id = task.payload.as_str()?;
    let title = if task.status == Status::Succeeded {
        "智慧教室签到成功"
    } else {
        "智慧教室签到失败"
    };
    let time = DateTime::from_timestamp(task.due_ms / 1_000).format();
    let mut body = format!("排课 ID {id}\n计划时间 {time}（北京时间）");
    if let Some(error) = task.last_error.as_deref() {
        body.push_str(&format!("\n{error}"));
    }
    Some(Message {
        title: title.into(),
        body,
        group: "智慧教室".into(),
    })
}
