//! Reading a list's pages, as a client follows them (API-05, API-06).

use serde_json::Value;

use super::{Api, users::SignedIn};

/// The values of `field` in each item of a list's body.
pub fn field(body: &Value, field: &str) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item[field].as_str().unwrap().to_owned())
        .collect()
}

/// The values of `field_name` on every page of the list at `path`, a path with a query, read as
/// `user` by following each page's `next_cursor`.
pub async fn pages(api: &Api, user: &SignedIn, path: &str, field_name: &str) -> Vec<Vec<String>> {
    let mut pages = Vec::new();
    let mut next = path.to_owned();
    loop {
        let body = api.get(&next).bearer(&user.access).send().await.json();
        pages.push(field(&body, field_name));
        match body["next_cursor"].as_str() {
            Some(cursor) => next = format!("{path}&cursor={cursor}"),
            None => return pages,
        }
    }
}
