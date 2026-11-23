//! The allowance of a day's uploads: each user's budget, in which every upload route counts the
//! bytes of the bodies that it reads (PLATFORM-093).

use super::{avatars, objects, own::upload_own, upload, workspace_pictures};
use crate::support::{
    Api,
    api::{self, Options},
    budgets::assert_refused_by,
    files, users,
};

/// The setting of the budget.
const SETTING: &str = "UPLOAD_USER_BYTES_PER_DAY";

/// The least that the budget is: the largest `FILE_MAX_BYTES`.
const LEAST: u32 = 20_971_520;

/// The budget's window, in seconds: a day.
const DAY: u64 = 86_400;

/// A text of 1,000 bytes: more than any bucket of requests counts in a test, so a count of 1,000
/// or more is a budget's.
fn text() -> Vec<u8> {
    vec![b'a'; 1_000]
}

/// The counts of the budgets that hold 1,000 bytes or more.
async fn budgets(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT count::text FROM rate_limit_buckets WHERE count >= 1000 ORDER BY count",
        )
        .await
}

#[tokio::test]
async fn platform_093_every_upload_route_counts_its_body_s_bytes_in_its_caller_s_budget() {
    let api = Api::new().await;
    let (ada, workspace) = workspace_pictures::owned(&api).await;
    let image = files::PNG.len();

    let own = upload_own(&api, &ada, "a.txt", &text(), "", None).await;
    let after_own = budgets(&api).await;
    let shared = upload(&api, &ada, &workspace, "a.txt", &text(), None).await;
    let after_shared = budgets(&api).await;
    let avatar = avatars::set(&api, &ada, files::PNG).await;
    let after_avatar = budgets(&api).await;
    let picture = workspace_pictures::set(&api, &ada, &workspace, files::PNG).await;
    let after_picture = budgets(&api).await;

    assert_eq!(
        [own.status, shared.status, avatar.status, picture.status],
        [201, 201, 200, 200]
    );
    assert_eq!(after_own, ["1000"]);
    assert_eq!(after_shared, ["2000"]);
    assert_eq!(after_avatar, [(2_000 + image).to_string()]);
    assert_eq!(after_picture, [(2_000 + 2 * image).to_string()]);
}

#[tokio::test]
async fn platform_093_an_upload_beyond_the_budget_answers_429_and_stores_nothing() {
    let api = Api::with(Options::default().set(SETTING, LEAST)).await;
    let (ada, workspace) = workspace_pictures::owned(&api).await;
    let grace = users::register(&api, "grace@example.com").await;
    assert_eq!(
        upload_own(&api, &ada, "a.txt", &text(), "", None)
            .await
            .status,
        201
    );
    // The window as it is once Ada's uploads of the day hold all of her budget but 100 bytes.
    api.database()
        .owner_executes(&format!(
            "UPDATE rate_limit_buckets SET count = {} WHERE count = 1000",
            LEAST - 100
        ))
        .await;
    let stored = objects(&api).await;

    let last_bytes = upload_own(&api, &ada, "b.txt", &[b'b'; 100], "", None).await;
    let within = objects(&api).await;
    let beyond = upload_own(&api, &ada, "c.txt", b"c", "", None).await;
    let shared = upload(&api, &ada, &workspace, "c.txt", b"c", None).await;
    let avatar = avatars::set(&api, &ada, files::PNG).await;
    let picture = workspace_pictures::set(&api, &ada, &workspace, files::PNG).await;
    let hers = upload_own(&api, &grace, "a.txt", &text(), "", None).await;

    assert_eq!(
        last_bytes.status, 201,
        "the budget's last byte is within it"
    );
    assert_eq!(within.len(), stored.len() + 1);
    for refused in [&beyond, &shared, &avatar, &picture] {
        assert_refused_by(refused, "uploads", LEAST, DAY);
    }
    assert_eq!(hers.status, 201, "each user has a budget of their own");
    assert_eq!(
        objects(&api).await.len(),
        within.len() + 1,
        "Grace's file alone"
    );
}

#[tokio::test]
async fn platform_093_a_body_counts_once_it_is_read_whatever_becomes_of_the_upload() {
    let api = Api::with(Options::default().set("FILE_MAX_BYTES", 2_000)).await;
    let ada = users::register(&api, "ada@example.com").await;

    // A kind that no upload takes, which the upload learns once it has read the body.
    let refused = upload_own(&api, &ada, "a.exe", &text(), "", None).await;
    let after_refused = budgets(&api).await;
    // A body over the route's limit, and a query that is refused with the body unread.
    let over = upload_own(&api, &ada, "a.txt", &[b'a'; 3_000], "", None).await;
    let unread = upload_own(&api, &ada, "a.txt", &text(), "?visibility=secret", None).await;

    assert_eq!(refused.status, 422, "{}", refused.json());
    assert_eq!(after_refused, ["1000"]);
    assert_eq!(over.status, 413);
    assert_eq!(unread.status, 400);
    assert_eq!(budgets(&api).await, ["1000"]);
}

#[tokio::test]
async fn platform_093_a_retry_of_a_keyed_upload_counts_its_bytes_again() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let first = upload_own(&api, &ada, "a.txt", &text(), "", Some("\"k1\"")).await;
    let retry = upload_own(&api, &ada, "a.txt", &text(), "", Some("\"k1\"")).await;

    assert_eq!(first.status, 201);
    assert_eq!(retry.status, 201);
    assert_eq!(retry.json()["id"], first.json()["id"]);
    assert_eq!(budgets(&api).await, ["2000"]);
}

#[tokio::test]
async fn platform_093_the_budget_is_a_gibibyte_unless_its_setting_says_otherwise() {
    let (server, _) = api::configuration(&Options::default());
    assert_eq!(server.rate_limits.upload_bytes, 1_073_741_824);

    for (set, read) in [(LEAST, LEAST), (2_147_483_646, 2_147_483_646)] {
        let (server, _) = api::configuration(&Options::default().set(SETTING, set));
        assert_eq!(server.rate_limits.upload_bytes, read);
    }
    for refused in [LEAST - 1, 2_147_483_647] {
        assert_eq!(
            api::refusal(&Options::default().set(SETTING, refused)),
            "UPLOAD_USER_BYTES_PER_DAY must be a whole number from 20971520 to 2147483646"
        );
    }
}
