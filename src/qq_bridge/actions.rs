//! Prepare exact operations from fixed commands, then run only on confirmation.

use std::collections::BTreeMap;
use std::path::Path;

use buaa_api::Context;
use buaa_api::api::tes::{Answer, Form, Task};
use buaa_api::time::DateTime;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::class_jobs::{self, Checkin};
use crate::notify;
use crate::server::{self, protocol, task::now_ms};

use super::queries::{api_error, class_records, day, evaluation_tasks, list};

pub(super) enum Action {
    Submit {
        kind: &'static str,
        payload: Value,
    },
    Configure {
        namespace: &'static str,
        payload: Value,
    },
    Server(bool),
    Bark(bool),
    TestNotification(bool),
    Wifi(bool),
    AutomaticEvaluation(Vec<Task>),
    Evaluation(Box<Evaluation>),
}

pub(super) struct Evaluation {
    task: Task,
    form: Form,
    answers: Vec<Answer>,
    reason: Option<String>,
}

impl Action {
    pub(super) async fn prepare(
        operation: &str,
        argument: &str,
        path: &Path,
        disable_tls: bool,
    ) -> Result<(String, Self), String> {
        let argument = argument.trim();
        let local: Option<(&str, Self)> = match (operation, argument) {
            ("server_start", "") => Some((
                "启动 Win 选课服务并恢复现有自动策略和任务。",
                Self::Server(true),
            )),
            ("server_stop", "") => {
                Some(("停止 Win 选课服务。未完成任务会保留。", Self::Server(false)))
            }
            ("bark_on", "") => Some(("恢复 Win 上的 Bark 通知。", Self::Bark(true))),
            ("bark_off", "") => Some(("暂停 Win 上的 Bark 通知。", Self::Bark(false))),
            ("test_notification", "" | "QQ") => Some((
                "向绑定的 QQ 私聊发送测试通知。",
                Self::TestNotification(false),
            )),
            ("test_notification", "Bark") => Some((
                "向 Win 配置的 Bark 设备发送测试通知。",
                Self::TestNotification(true),
            )),
            ("wifi_on", "") => Some(("使用 Win WSL 当前网络登录校园网。", Self::Wifi(true))),
            ("wifi_off", "") => Some((
                "注销 Win WSL 当前网络的校园网会话，可能暂时切断网络。",
                Self::Wifi(false),
            )),
            _ => None,
        };
        if let Some((preview, action)) = local {
            return Ok((preview.into(), action));
        }
        match operation {
            "auto_select" => {
                let targets = targets(argument)?;
                let preview = list(
                    "更新博雅自动选课目标（0 表示关闭该类别）：",
                    super::CATEGORIES
                        .iter()
                        .filter_map(|(key, label)| {
                            targets.get(key).map(|count| format!("{label} {count}"))
                        })
                        .collect(),
                );
                return Ok((
                    preview,
                    Self::Configure {
                        namespace: "boya.auto.select",
                        payload: json!(targets),
                    },
                ));
            }
            "auto_sign" => {
                let enabled = match argument {
                    "开" | "开启" => true,
                    "关" | "关闭" => false,
                    _ => return Err("请用 /博雅 自动签到 开 或 /博雅 自动签到 关。".into()),
                };
                let mode = if enabled { "开启" } else { "关闭" };
                return Ok((
                    format!("{mode}博雅自动签到和签退。"),
                    Self::Configure {
                        namespace: "boya.auto.sign",
                        payload: json!({"enabled": enabled}),
                    },
                ));
            }
            "boya_select" | "boya_drop" | "boya_sign" | "class_sign" | "class_auto"
            | "tes_auto" | "tes_submit" => {}
            _ => return Err("无效的写操作或参数。".into()),
        }
        let session = super::session(path, disable_tls)?;
        let context = &session.context;
        let result = match operation {
            "boya_select" | "boya_drop" | "boya_sign" => boya(context, operation, argument).await,
            "class_sign" | "class_auto" => {
                class(
                    context,
                    if operation == "class_auto" {
                        ""
                    } else {
                        argument
                    },
                )
                .await
            }
            "tes_auto" if argument.is_empty() => {
                let tasks: Vec<_> = evaluation_tasks(context)
                    .await?
                    .into_iter()
                    .filter(|task| !task.state)
                    .collect();
                if tasks.is_empty() {
                    return Err("暂无待评教任务。".into());
                }
                let preview = list(
                    "按 CLI 的默认评教答案提交以下任务：",
                    tasks
                        .iter()
                        .map(|t| format!("{} · {}", t.course, t.teacher))
                        .collect(),
                );
                Ok((preview, Self::AutomaticEvaluation(tasks)))
            }
            "tes_submit" => evaluation(context, argument).await,
            _ => Err("无效的写操作或参数。".into()),
        };
        let _ = session.save(false);
        result
    }

    pub(super) async fn execute(self, path: &Path, disable_tls: bool) -> Result<String, String> {
        let dir = server::directory(path);
        match self {
            Self::Submit { kind, payload } => {
                ipc(
                    path,
                    protocol::Request::Submit {
                        kind: kind.into(),
                        payload,
                    },
                )
                .await
            }
            Self::Configure { namespace, payload } => {
                ipc(
                    path,
                    protocol::Request::Configure {
                        namespace: namespace.into(),
                        payload,
                    },
                )
                .await
            }
            Self::Server(true) => {
                // The Win deployment uses a separate systemd unit so restarting
                // the bridge does not terminate a daemon started from QQ.
                if protocol::call(&dir, protocol::Request::Status)
                    .await
                    .is_ok()
                {
                    return Ok("Win 选课服务正在运行。".into());
                }
                if std::env::var_os("BUAA_QQ_SYSTEMD").is_some() {
                    let result = tokio::process::Command::new("systemctl")
                        .args(["--user", "start", "buaa-daemon.service"])
                        .output()
                        .await
                        .map_err(|_| "无法启动 Win 选课服务。")?;
                    if !result.status.success() {
                        return Err("无法启动 Win 选课服务；请检查 WSL 用户服务。".into());
                    }
                    for _ in 0..30 {
                        if protocol::call(&dir, protocol::Request::Status)
                            .await
                            .is_ok()
                        {
                            return Ok("Win 选课服务已启动。".into());
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                    return Err("服务启动后未通过状态检查。".into());
                }
                server::start(path, disable_tls)
                    .await
                    .map_err(|_| "无法启动 Win 选课服务。")?;
                Ok("Win 选课服务已启动。".into())
            }
            Self::Server(false) => ipc(path, protocol::Request::Stop).await,
            Self::Bark(enabled) => {
                notify::set_bark(&dir, enabled)
                    .map_err(|_| "无法更新 Bark。请检查 WSL 的 Bark 配置。")?;
                Ok(if enabled {
                    "Win Bark 已开启。"
                } else {
                    "Win Bark 已暂停。"
                }
                .into())
            }
            Self::TestNotification(true) => {
                notify::test_bark(&dir)
                    .await
                    .map_err(|_| "Bark 测试通知投递失败。")?;
                Ok("Bark 已接受测试通知。".into())
            }
            Self::TestNotification(false) => {
                notify::test_qq(&dir).map_err(|_| "无法将 QQ 测试通知入队。")?;
                Ok("QQ 测试通知已入队。".into())
            }
            action => {
                let session = super::session(path, disable_tls)?;
                let context = &session.context;
                let result = match action {
                    Self::Wifi(true) => context
                        .wifi()
                        .login()
                        .await
                        .map(|_| "已提交校园网登录。".into())
                        .map_err(api_error),
                    Self::Wifi(false) => context
                        .wifi()
                        .logout()
                        .await
                        .map(|_| "校园网已注销。".into())
                        .map_err(api_error),
                    Self::AutomaticEvaluation(tasks) => automatic_evaluation(context, tasks).await,
                    Self::Evaluation(prepared) => submit_evaluation(context, *prepared).await,
                    _ => unreachable!(),
                };
                let _ = session.save(false);
                result
            }
        }
    }
}

async fn ipc(path: &Path, request: protocol::Request) -> Result<String, String> {
    let response = protocol::call(&server::directory(path), request)
        .await
        .map_err(|_| "Win 选课服务不可用。")?;
    if response.ok {
        Ok(response.message)
    } else {
        Err(response.message)
    }
}

fn targets(input: &str) -> Result<BTreeMap<&'static str, u32>, &'static str> {
    let mut targets = BTreeMap::new();
    for pair in input.split([',', '，']) {
        let (key, count) = pair
            .trim()
            .split_once('=')
            .ok_or("请用 德育=3,美育=1 的格式。")?;
        let &(key, _) = super::CATEGORIES
            .iter()
            .find(|(code, label)| *code == key.trim() || *label == key.trim())
            .ok_or("未知的博雅类别。")?;
        let count = count
            .trim()
            .parse::<u32>()
            .map_err(|_| "目标必须是非负整数。")?;
        if targets.insert(key, count).is_some() {
            return Err("请为每个类别只提供一个目标。");
        }
    }
    Ok(targets)
}

async fn boya(
    context: &Context,
    operation: &str,
    argument: &str,
) -> Result<(String, Action), String> {
    let id = argument
        .parse::<u32>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or("请提供正整数博雅课程 ID。")?;
    let c = context.boya().query_course(id).await.map_err(api_error)?;
    let (description, kind) = match operation {
        "boya_select" => (
            format!(
                "选课《{}》（{id}）。\n选课窗口：{} 至 {}。",
                c.name,
                c.schedule.select_start.format(),
                c.schedule.select_end.format()
            ),
            "boya.select.plan",
        ),
        "boya_drop" => (
            format!("退课《{}》（{id}）。自动选课目标仍会用于后续补选。", c.name),
            "boya.drop",
        ),
        "boya_sign" => (
            format!("为《{}》（{id}）安排学校窗口内的签到与签退。", c.name),
            "boya.sign.plan",
        ),
        _ => unreachable!(),
    };
    Ok((
        description,
        Action::Submit {
            kind,
            payload: json!({"id": id}),
        },
    ))
}

async fn class(context: &Context, argument: &str) -> Result<(String, Action), String> {
    let (preview, jobs) = if class_jobs::valid_schedule(argument) {
        (
            format!("尝试智慧教室签到，排课 ID {argument}。"),
            vec![Checkin {
                schedule: argument.into(),
                due_ms: now_ms(),
            }],
        )
    } else {
        let date = day(argument)?;
        let schedules = class_records(context.class().query_schedule(&date).await)?;
        let mut jobs = Vec::new();
        let mut items = Vec::new();
        for schedule in schedules.into_iter().filter(|schedule| !schedule.status) {
            let due_ms = (schedule.time.timestamp() * 1000 - 600_000).max(now_ms());
            items.push(format!(
                "{} · 排课 {} · 计划 {}",
                schedule.name,
                schedule.id,
                DateTime::from_timestamp(due_ms / 1000).format()
            ));
            jobs.push(Checkin {
                schedule: schedule.id,
                due_ms,
            });
        }
        if jobs.is_empty() {
            return Err("该日期暂无待签到排课。".into());
        }
        (list("安排以下智慧教室签到（北京时间）：", items), jobs)
    };
    Ok((
        preview,
        Action::Submit {
            kind: class_jobs::CHECKIN,
            payload: json!(jobs),
        },
    ))
}

pub(super) async fn evaluation_task(context: &Context, argument: &str) -> Result<Task, String> {
    let index = argument
        .parse::<usize>()
        .ok()
        .and_then(|index| index.checked_sub(1))
        .ok_or("请先查评教列表，再提供列表中的序号。")?;
    evaluation_tasks(context)
        .await?
        .into_iter()
        .nth(index)
        .filter(|task| !task.state)
        .ok_or_else(|| "评教序号不存在或该任务已完成。".into())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FormInput {
    answers: Vec<Value>,
    reason: Option<String>,
}

async fn evaluation(context: &Context, argument: &str) -> Result<(String, Action), String> {
    let (index, answers) = argument
        .split_once(char::is_whitespace)
        .ok_or("格式：/北航 交评教 序号 {\"answers\":[1,2,null],\"reason\":\"原因（需要时）\"}")?;
    let input: FormInput =
        serde_json::from_str(answers).map_err(|_| "答案须为 JSON，包含 answers 和可选 reason。")?;
    let task = evaluation_task(context, index).await?;
    let form = context.tes().get_form(&task).await.map_err(api_error)?;
    let answers = checked_answers(&form, input.answers)?;
    let result = form.fill(answers.clone());
    if (result.is_unqualified() || result.is_perfect())
        && !input
            .reason
            .as_deref()
            .is_some_and(|reason| (10..=200).contains(&reason.chars().count()))
    {
        return Err("满分或不及格评教需填写 10–200 字的 reason。".into());
    }
    let preview = format!(
        "提交评教《{}》 · {}，得分 {:.1}。",
        task.course,
        task.teacher,
        result.score()
    );
    drop(result);
    Ok((
        preview,
        Action::Evaluation(Box::new(Evaluation {
            task,
            form,
            answers,
            reason: input.reason,
        })),
    ))
}

fn checked_answers(form: &Form, supplied: Vec<Value>) -> Result<Vec<Answer>, String> {
    if supplied.len() != form.questions.len() {
        return Err(format!(
            "这份问卷有 {} 道题，答案数量需要一致。",
            form.questions.len()
        ));
    }
    form.questions
        .iter()
        .zip(supplied)
        .enumerate()
        .map(|(index, (q, answer))| {
            if q.is_choice {
                let i = answer
                    .as_u64()
                    .and_then(|v| usize::try_from(v).ok())
                    .and_then(|v| v.checked_sub(1))
                    .filter(|i| *i < q.choices.len())
                    .ok_or_else(|| format!("第 {} 题的选项无效。", index + 1))?;
                Ok(Answer::Choice(i))
            } else if q.choices.is_empty() {
                Err(format!("学校返回的第 {} 题格式不完整。", index + 1))
            } else if answer.is_null() {
                Ok(Answer::Completion(None))
            } else {
                let text = answer
                    .as_str()
                    .ok_or_else(|| format!("第 {} 题请填写文字或 null。", index + 1))?;
                Ok(Answer::Completion(Some(
                    text.replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('>', "&gt;"),
                )))
            }
        })
        .collect()
}

fn still_pending(task: &Task, latest: &[Task]) -> bool {
    serde_json::to_value(task).is_ok_and(|key| {
        latest
            .iter()
            .any(|t| !t.state && serde_json::to_value(t).is_ok_and(|v| v == key))
    })
}

async fn submit_evaluation(context: &Context, evaluation: Evaluation) -> Result<String, String> {
    let tes = context.tes();
    if !still_pending(&evaluation.task, &evaluation_tasks(context).await?) {
        return Err("该评教任务已完成或已变更，请重新查询。".into());
    }
    let mut completed = evaluation.form.fill(evaluation.answers);
    if let Some(reason) = evaluation.reason {
        completed
            .set_reason(reason)
            .map_err(|_| "评教原因长度无效。")?;
    }
    tes.submit_form(completed).await.map_err(api_error)?;
    Ok("学校系统已接受评教。".into())
}

async fn automatic_evaluation(context: &Context, tasks: Vec<Task>) -> Result<String, String> {
    let latest = evaluation_tasks(context).await?;
    let mut reports = Vec::new();
    for task in tasks {
        let status = if still_pending(&task, &latest) {
            auto_submit(context, &task).await.unwrap_or_else(|err| err)
        } else {
            "已完成或已变更，跳过".into()
        };
        reports.push(format!("{} · {} · {status}", task.course, task.teacher));
    }
    Ok(list("批量评教结果", reports))
}

async fn auto_submit(context: &Context, task: &Task) -> Result<String, String> {
    let tes = context.tes();
    let form = tes.get_form(task).await.map_err(api_error)?;
    let mut first = true;
    for q in &form.questions {
        let required = if q.is_choice && std::mem::take(&mut first) {
            2
        } else {
            1
        };
        if q.choices.len() < required {
            return Err("问卷选项不完整，跳过".into());
        }
    }
    let complete = form.fill_default();
    if complete.is_perfect() || complete.is_unqualified() {
        return Err("默认答案需要评教原因，请手动填写".into());
    }
    let score = complete.score();
    tes.submit_form(complete).await.map_err(api_error)?;
    Ok(format!("已提交 {score:.1} 分"))
}
