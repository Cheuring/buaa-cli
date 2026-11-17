//! Boya policies and handlers; scheduler/transport contain no course logic.

use buaa_api::Context;
use buaa_api::api::boya::{Category, Course, SignConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::server::Handler;
use crate::server::task::{Outcome, State, Task, now_ms};

const SELECT: &str = "boya.auto.select";
const SIGN: &str = "boya.auto.sign";
const DISCOVER_INTERVAL: i64 = 60_000;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    pub targets: BTreeMap<String, u32>,
    #[serde(default)]
    pub auto_sign: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CourseTask {
    id: u32,
    #[serde(default)]
    checkout: bool,
}

pub struct BoyaHandler {
    pub data_dir: PathBuf,
    pub disable_tls: bool,
}

fn policy(settings: &Value) -> Result<Policy, String> {
    serde_json::from_value(settings.get("boya").cloned().unwrap_or_else(|| json!({})))
        .map_err(|e| e.to_string())
}

fn category_key(category: Category) -> &'static str {
    match category {
        Category::Arts => "arts",
        Category::Ethics => "ethics",
        Category::Labor => "labor",
        Category::Safety => "safety",
        Category::Other => "other",
    }
}

fn discover(group: &str) -> Task {
    let mut task = Task::new(
        format!("{group}.discover"),
        &format!("{group}.discover"),
        json!({}),
        now_ms(),
    );
    task.group = Some(group.into());
    task.priority = 0;
    task.preemptible = true;
    task.max_attempts = None;
    task
}

impl Handler for BoyaHandler {
    fn configure(
        &self,
        state: &mut State,
        namespace: &str,
        payload: Value,
    ) -> Result<String, String> {
        let mut config = policy(&state.settings)?;
        let message = match namespace {
            SELECT => {
                let targets: BTreeMap<String, u32> =
                    serde_json::from_value(payload).map_err(|e| e.to_string())?;
                if targets.is_empty()
                    || targets.keys().any(|key| {
                        !["arts", "ethics", "labor", "safety", "other"].contains(&key.as_str())
                    })
                {
                    return Err("Specify at least one valid category target".into());
                }
                for (category, count) in targets {
                    if count == 0 {
                        config.targets.remove(&category);
                    } else {
                        config.targets.insert(category, count);
                    }
                }
                state.cancel_group(SELECT);
                if !config.targets.is_empty() {
                    state.enqueue(discover(SELECT));
                }
                format!(
                    "Boya semester selection targets: {} (0 disables a category)",
                    json!(config.targets)
                )
            }
            SIGN => {
                config.auto_sign = !config.auto_sign;
                state.cancel_group(SIGN);
                if config.auto_sign {
                    state.enqueue(discover(SIGN));
                }
                format!(
                    "Boya automatic check-in/out {}",
                    if config.auto_sign {
                        "enabled"
                    } else {
                        "disabled"
                    }
                )
            }
            _ => return Err(format!("Unknown configuration namespace: {namespace}")),
        };
        state.settings["boya"] = serde_json::to_value(config).map_err(|e| e.to_string())?;
        Ok(message)
    }

    fn submit(&self, state: &mut State, kind: &str, payload: Value) -> Result<String, String> {
        if kind != "boya.sign.plan" {
            return Err(format!("Unknown submitted task kind: {kind}"));
        }
        let course: CourseTask = serde_json::from_value(payload).map_err(|e| e.to_string())?;
        if course.id == 0 {
            return Err("Course ID must be positive".into());
        }
        let key = format!("boya.sign.plan:{}", course.id);
        // Resubmission intentionally refreshes the remote course/sign windows.
        state.tasks.retain(|task| task.key != key);
        state.enqueue(Task::new(key, kind, json!(course), now_ms()));
        Ok(format!(
            "Submitted check-in/out planning for course {}; inspect `buaa server status` for the result",
            course.id
        ))
    }

    async fn execute(&self, task: Task, settings: Value) -> Result<Outcome, String> {
        let auth_path = self.data_dir.join("cred.json");
        let original_auth = std::fs::read(&auth_path).ok();
        // Rebuild for every task, picking up an interactive login since last run.
        let context = crate::auth::context(&self.data_dir, self.disable_tls, false);
        let result = execute(&context, &task, policy(&settings)?).await;
        // Do not overwrite credentials replaced by an interactive login while
        // this task was running. State ownership is otherwise in the daemon.
        if std::fs::read(&auth_path).ok() == original_auth {
            context
                .save_auth(&self.data_dir)
                .map_err(|e| format!("Failed to save auth: {e}"))?;
        }
        result
    }
}

async fn execute(context: &Context, task: &Task, config: Policy) -> Result<Outcome, String> {
    match task.kind.as_str() {
        "boya.auto.select.discover" => select_plan(context, &config).await,
        "boya.auto.sign.discover" => sign_discover(context).await,
        "boya.select" => select_one(context, task, &config).await,
        "boya.sign.plan" => {
            let payload: CourseTask =
                serde_json::from_value(task.payload.clone()).map_err(|e| e.to_string())?;
            let course = context
                .boya()
                .query_course(payload.id)
                .await
                .map_err(|e| e.to_string())?;
            ensure_selected(context, &course).await?;
            let tasks = sign_tasks(&course, None, now_ms())?;
            if tasks.is_empty() {
                return Err("All check-in/out windows have passed".into());
            }
            Ok(Outcome {
                tasks,
                ..Outcome::default()
            })
        }
        "boya.sign" => sign_one(context, task).await,
        _ => Err(format!("Unknown task kind: {}", task.kind)),
    }
}

async fn selected(context: &Context) -> Result<Vec<buaa_api::api::boya::Selected>, String> {
    let semester = context
        .boya()
        .get_semester()
        .await
        .map_err(|e| e.to_string())?;
    context
        .boya()
        .query_selected(semester)
        .await
        .map_err(|e| e.to_string())
}

fn counts(courses: &[buaa_api::api::boya::Selected]) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    for course in courses {
        *counts
            .entry(category_key(course.category).into())
            .or_default() += 1;
    }
    counts
}

fn selectable(
    course: &Course,
    targets: &BTreeMap<String, u32>,
    counts: &BTreeMap<String, u32>,
    now: i64,
) -> bool {
    let category = category_key(course.category);
    !course.selected
        && course.sign_config.is_some()
        && course.capacity.current < course.capacity.max
        && course.schedule.select_end.timestamp() * 1000 > now
        && counts.get(category).copied().unwrap_or(0) < targets.get(category).copied().unwrap_or(0)
}

async fn select_plan(context: &Context, config: &Policy) -> Result<Outcome, String> {
    let counts = counts(&selected(context).await?);
    if config
        .targets
        .iter()
        .all(|(category, target)| counts.get(category).copied().unwrap_or(0) >= *target)
    {
        return Ok(Outcome {
            repeat_after_ms: Some(DISCOVER_INTERVAL),
            ..Outcome::default()
        });
    }
    let mut tasks = Vec::new();
    let mut seen = HashSet::new();
    // The pinned API uses u8 page numbers and page sizes. Scan until the first
    // short page, and reject silent truncation if the API's range is exhausted.
    for page in 1..=u8::MAX {
        let courses = context
            .boya()
            .query_courses(page, 100)
            .await
            .map_err(|e| e.to_string())?;
        let last = courses.len() < 100;
        for course in courses {
            if !seen.insert(course.id) {
                continue;
            }
            if selectable(&course, &config.targets, &counts, now_ms()) {
                let due = (course.schedule.select_start.timestamp() * 1000).max(now_ms());
                let mut task = Task::new(
                    format!("boya.select:{}", course.id),
                    "boya.select",
                    json!(CourseTask {
                        id: course.id,
                        checkout: false
                    }),
                    due,
                );
                task.expires_ms = Some(course.schedule.select_end.timestamp() * 1000);
                task.group = Some(SELECT.into());
                task.max_attempts = None;
                tasks.push(task);
            }
        }
        if last {
            return Ok(Outcome {
                tasks,
                repeat_after_ms: Some(DISCOVER_INTERVAL),
            });
        }
    }
    Err("Course pagination exceeded the API's 255-page limit".into())
}

async fn select_one(context: &Context, task: &Task, config: &Policy) -> Result<Outcome, String> {
    let payload: CourseTask =
        serde_json::from_value(task.payload.clone()).map_err(|e| e.to_string())?;
    let course = context
        .boya()
        .query_course(payload.id)
        .await
        .map_err(|e| e.to_string())?;
    let chosen = selected(context).await?;
    // Reconcile after uncertain results/restarts and before EVERY enrollment.
    // This serial execution prevents multiple candidates overfilling a target.
    if course.selected || chosen.iter().any(|c| c.id == course.id) {
        return Ok(Outcome::default());
    }
    if !selectable(&course, &config.targets, &counts(&chosen), now_ms()) {
        return Ok(Outcome::default());
    }
    let due = course.schedule.select_start.timestamp() * 1000;
    if now_ms() < due {
        return Ok(Outcome {
            repeat_after_ms: Some(due - now_ms()),
            ..Outcome::default()
        });
    }
    context
        .boya()
        .select_course(course.id)
        .await
        .map_err(|e| e.to_string())?;
    // select_course's upstream implementation does not parse its status body.
    // Verify remote enrollment rather than interpreting an HTTP response as success.
    if !selected(context).await?.iter().any(|c| c.id == course.id) {
        return Err("Enrollment was not confirmed by the selected course list".into());
    }
    let tasks = if config.auto_sign {
        sign_tasks(&course, Some(SIGN), now_ms())?
    } else {
        Vec::new()
    };
    Ok(Outcome {
        tasks,
        ..Outcome::default()
    })
}

async fn ensure_selected(context: &Context, course: &Course) -> Result<(), String> {
    if course.selected || selected(context).await?.iter().any(|c| c.id == course.id) {
        Ok(())
    } else {
        Err("Course is not selected; select it before scheduling check-in/out".into())
    }
}

fn sign_tasks(course: &Course, group: Option<&str>, now: i64) -> Result<Vec<Task>, String> {
    let config = course
        .sign_config
        .as_ref()
        .ok_or("Course has no self-service sign_config")?;
    let mut tasks = Vec::new();
    for (checkout, start, end) in [
        (false, config.checkin_start, config.checkin_end),
        (true, config.checkout_start, config.checkout_end),
    ] {
        let due = start.timestamp() * 1000;
        let expiry = end.timestamp() * 1000;
        if expiry <= now || due >= expiry {
            continue;
        }
        let phase = if checkout { "out" } else { "in" };
        let mut task = Task::new(
            format!("boya.sign:{}:{phase}:{due}", course.id),
            "boya.sign",
            json!(CourseTask {
                id: course.id,
                checkout
            }),
            due.max(now),
        );
        task.expires_ms = Some(expiry);
        task.group = group.map(str::to_owned);
        tasks.push(task);
    }
    Ok(tasks)
}

async fn sign_discover(context: &Context) -> Result<Outcome, String> {
    let mut tasks = Vec::new();
    for chosen in selected(context).await? {
        if chosen.schedule.course_end.timestamp() * 1000 + 86_400_000 < now_ms() {
            continue;
        }
        let course = context
            .boya()
            .query_course(chosen.id)
            .await
            .map_err(|e| e.to_string())?;
        if course.sign_config.is_some() {
            tasks.extend(sign_tasks(&course, Some(SIGN), now_ms())?);
        }
    }
    Ok(Outcome {
        tasks,
        repeat_after_ms: Some(DISCOVER_INTERVAL),
    })
}

fn sign_window(config: &SignConfig, checkout: bool) -> (i64, i64) {
    let (start, end) = if checkout {
        (config.checkout_start, config.checkout_end)
    } else {
        (config.checkin_start, config.checkin_end)
    };
    (start.timestamp() * 1000, end.timestamp() * 1000)
}

async fn sign_one(context: &Context, task: &Task) -> Result<Outcome, String> {
    let payload: CourseTask =
        serde_json::from_value(task.payload.clone()).map_err(|e| e.to_string())?;
    let course = context
        .boya()
        .query_course(payload.id)
        .await
        .map_err(|e| e.to_string())?;
    ensure_selected(context, &course).await?;
    let config = course
        .sign_config
        .ok_or("Course no longer supports self-service signing")?;
    let (start, end) = sign_window(&config, payload.checkout);
    let now = now_ms();
    if now >= end {
        return Err("The remote signing window has passed".into());
    }
    if now < start {
        return Ok(Outcome {
            repeat_after_ms: Some(start - now),
            ..Outcome::default()
        });
    }
    let result = if payload.checkout {
        context
            .boya()
            .checkout_course(payload.id, &config.coordinate)
            .await
    } else {
        context
            .boya()
            .checkin_course(payload.id, &config.coordinate)
            .await
    }
    .map_err(|e| e.to_string())?;
    if !result.is_ok {
        return Err("Boya rejected the sign location/result".into());
    }
    Ok(Outcome::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use buaa_api::api::boya::{Capacity, Coordinate, Schedule};
    use buaa_api::time::DateTime;

    fn course(id: u32, category: Category, can_sign: bool) -> Course {
        let dt = DateTime::from_timestamp;
        Course {
            id,
            name: "Test".into(),
            location: "Test".into(),
            category,
            selected: false,
            campuses: Vec::new(),
            capacity: Capacity {
                current: 0,
                max: 10,
            },
            schedule: Schedule {
                course_start: dt(300),
                course_end: dt(400),
                select_start: dt(100),
                select_end: dt(200),
                cancel_end: dt(250),
            },
            sign_config: can_sign.then_some(SignConfig {
                checkin_start: dt(300),
                checkin_end: dt(320),
                checkout_start: dt(380),
                checkout_end: dt(400),
                coordinate: Coordinate {
                    longitude: 0.0,
                    latitude: 0.0,
                    radius: 1.0,
                },
            }),
        }
    }

    #[test]
    fn self_service_only_and_semester_quota() {
        let targets = BTreeMap::from([("arts".into(), 5)]);
        let counts = BTreeMap::from([("arts".into(), 4)]);
        assert!(selectable(
            &course(1, Category::Arts, true),
            &targets,
            &counts,
            150_000
        ));
        assert!(!selectable(
            &course(2, Category::Arts, false),
            &targets,
            &counts,
            150_000
        ));
        assert!(!selectable(
            &course(3, Category::Labor, true),
            &targets,
            &counts,
            150_000
        ));
        assert!(!selectable(
            &course(1, Category::Arts, true),
            &targets,
            &BTreeMap::from([("arts".into(), 5)]),
            150_000
        ));
        assert!(!selectable(
            &course(1, Category::Arts, true),
            &targets,
            &counts,
            200_000
        ));
    }

    #[test]
    fn sign_windows_and_independent_manual_tasks() {
        let course = course(1, Category::Arts, true);
        let tasks = sign_tasks(&course, Some(SIGN), 310_000).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].due_ms, 310_000);
        assert_eq!(sign_tasks(&course, None, 320_000).unwrap().len(), 1);
        assert!(sign_tasks(&course, None, 400_000).unwrap().is_empty());
        let handler = BoyaHandler {
            data_dir: PathBuf::new(),
            disable_tls: false,
        };
        let mut state = State::default();
        handler.configure(&mut state, SIGN, json!({})).unwrap();
        assert!(policy(&state.settings).unwrap().auto_sign);
        state.enqueue(tasks[0].clone());
        state.enqueue(sign_tasks(&course, None, 310_000).unwrap().remove(0));
        handler.configure(&mut state, SIGN, json!({})).unwrap();
        assert!(!policy(&state.settings).unwrap().auto_sign);
        assert_eq!(state.tasks.len(), 1);
        assert!(state.tasks[0].group.is_none());
    }

    #[test]
    fn target_updates_preserve_other_categories_and_zero_disables() {
        let handler = BoyaHandler {
            data_dir: PathBuf::new(),
            disable_tls: false,
        };
        let mut state = State::default();
        handler
            .configure(&mut state, SELECT, json!({"arts": 5, "labor": 3}))
            .unwrap();
        handler
            .configure(&mut state, SELECT, json!({"arts": 0}))
            .unwrap();
        assert_eq!(
            policy(&state.settings).unwrap().targets,
            BTreeMap::from([("labor".into(), 3)])
        );
        assert!(
            handler
                .configure(&mut state, SELECT, json!({"typo": 1}))
                .is_err()
        );
    }
}
