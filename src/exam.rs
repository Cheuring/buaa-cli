use buaa_api::Context;
use buaa_api::api::app::{Exam, Exams};
use serde::Deserialize;

pub async fn exam(context: &Context) {
    match query(context).await {
        Ok(exams) => {
            let mut builder = tabled::builder::Builder::new();
            builder.push_record(["Name", "Start", "End", "Position"]);
            for e in exams {
                builder.push_record([
                    &e.name,
                    &e.start.to_string(),
                    &e.end.to_string(),
                    &e.position,
                ]);
            }
            crate::utils::print_table(builder);
        }
        Err(QueryError::Request(error)) => {
            eprintln!("[Error]::<Exam>: Failed to get exam list: {error}");
        }
        Err(QueryError::Data) => {
            eprintln!("[Error]::<Exam>: Failed to parse exam data");
        }
    }
}

pub(crate) enum QueryError {
    Request(buaa_api::Error),
    Data,
}

pub(crate) async fn query(context: &Context) -> Result<Vec<Exam>, QueryError> {
    let page = context
        .app()
        .universal_request("https://app.buaa.edu.cn/exam/wap/default/index")
        .await
        .map_err(QueryError::Request)?;
    parse(&page).ok_or(QueryError::Data)
}

fn parse(page: &[u8]) -> Option<Vec<Exam>> {
    // The website embeds the schedule in JavaScript. It sends an array when
    // there is no grouped schedule, and can send a term-keyed object.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Schedule {
        Grouped(Exams),
        Flat(Vec<Exam>),
    }

    let text = std::str::from_utf8(page).ok()?;
    let (_, definition) = text.split_once("\n        data: ")?;
    let (data, _) = definition.split_once(",\n")?;
    match serde_json::from_str::<Schedule>(data).ok()? {
        Schedule::Grouped(exams) => Some(exams.data),
        Schedule::Flat(mut exams) => {
            exams.sort_by_key(|exam| exam.start);
            Some(exams)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn exam_schedule_can_be_an_empty_array_or_a_grouped_object() {
        assert!(parse(b"\n        data: [],\n").unwrap().is_empty());
        assert!(
            parse(b"\n        data: {\"term\":[]},\n")
                .unwrap()
                .is_empty()
        );
        assert!(parse(b"<html>SSO</html>").is_none());
    }
}
