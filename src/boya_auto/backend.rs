//! Keep university transport behind the small interface used by policy handlers.
use buaa_api::Context;
use buaa_api::api::boya::{Coordinate, Course, Selected, SignInfo};
use std::future::Future;

type Result<T> = std::result::Result<T, String>;

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
        let semester = self
            .boya()
            .get_semester()
            .await
            .map_err(|e| e.to_string())?;
        self.boya()
            .query_selected(semester)
            .await
            .map_err(|e| e.to_string())
    }
    async fn courses(&self, page: u8) -> Result<Vec<Course>> {
        self.boya()
            .query_courses(page, 100)
            .await
            .map_err(|e| e.to_string())
    }
    async fn course(&self, id: u32) -> Result<Course> {
        self.boya()
            .query_course(id)
            .await
            .map_err(|e| e.to_string())
    }
    async fn enroll(&self, id: u32) -> Result<()> {
        self.boya()
            .select_course(id)
            .await
            .map_err(|e| e.to_string())
    }
    async fn sign(&self, id: u32, coordinate: &Coordinate, checkout: bool) -> Result<SignInfo> {
        if checkout {
            self.boya().checkout_course(id, coordinate).await
        } else {
            self.boya().checkin_course(id, coordinate).await
        }
        .map_err(|e| e.to_string())
    }
}
