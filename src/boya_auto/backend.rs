//! Keep university transport behind the small interface used by policy handlers.
use buaa_api::Context;
use buaa_api::api::boya::{Coordinate, Course, Selected, SignInfo};
use buaa_api::error::Kind;
use serde_json::{Value, json};
use std::future::Future;
use std::time::Instant;

use crate::server::logging::task_event;

type Result<T> = std::result::Result<T, String>;

async fn request<T>(
    operation: &str,
    target: Value,
    call: impl Future<Output = buaa_api::Result<T>>,
) -> Result<T> {
    task_event(
        "info",
        "boya.request.started",
        json!({
            "operation": operation, "target": target
        }),
    );
    let start = Instant::now();
    let result = call.await.map_err(|error| {
        // The API's Display may include raw server messages and a reqwest URL
        // containing login material. Only record the typed classification.
        let mut message = format!("{operation}: {}", error.kind());
        if let Some(code) = error.code() {
            message.push_str(&format!(" ({code:?})"));
        }
        if matches!(error.kind(), Kind::Auth) {
            message.push_str("; run `buaa login` interactively");
        }
        message
    });
    task_event(
        if result.is_ok() { "info" } else { "warn" },
        "boya.request.finished",
        json!({"operation": operation, "target": target,
            "elapsed_ms": start.elapsed().as_millis() as u64,
            "returned": if result.is_ok() { "ok" } else { "error" },
            "error": result.as_ref().err()}),
    );
    result
}

pub(super) trait Backend: Sync {
    fn selected(&self) -> impl Future<Output = Result<Vec<Selected>>> + Send;
    fn courses(&self, page: u8) -> impl Future<Output = Result<Vec<Course>>> + Send;
    fn course(&self, id: u32) -> impl Future<Output = Result<Course>> + Send;
    fn enroll(&self, id: u32) -> impl Future<Output = Result<()>> + Send;
    fn sign(
        &self,
        id: u32,
        coordinate: &Coordinate,
        checkout: bool,
    ) -> impl Future<Output = Result<SignInfo>> + Send;
}

impl Backend for Context {
    async fn selected(&self) -> Result<Vec<Selected>> {
        let boya = self.boya();
        let semester = request("get_semester", Value::Null, boya.get_semester()).await?;
        request("query_selected", Value::Null, boya.query_selected(semester)).await
    }
    async fn courses(&self, page: u8) -> Result<Vec<Course>> {
        request(
            "query_courses",
            json!({"page": page}),
            self.boya().query_courses(page, 100),
        )
        .await
    }
    async fn course(&self, id: u32) -> Result<Course> {
        request(
            "query_course",
            json!({"course_id": id}),
            self.boya().query_course(id),
        )
        .await
    }
    async fn enroll(&self, id: u32) -> Result<()> {
        request(
            "select_course",
            json!({"course_id": id}),
            self.boya().select_course(id),
        )
        .await
    }
    async fn sign(&self, id: u32, coordinate: &Coordinate, checkout: bool) -> Result<SignInfo> {
        let boya = self.boya();
        let target = json!({"course_id": id});
        if checkout {
            request("check_out", target, boya.checkout_course(id, coordinate)).await
        } else {
            request("check_in", target, boya.checkin_course(id, coordinate)).await
        }
    }
}
