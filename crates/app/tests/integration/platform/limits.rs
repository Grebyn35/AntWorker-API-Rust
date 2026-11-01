//! The client IP and the rate limits (PLATFORM-026, PLATFORM-047 to PLATFORM-051).

use crate::support::{Api, Options, users};

/// The API with an IP limit of `ip` and a user limit of `user` requests a window.
async fn limited(ip: u32, user: u32) -> Api {
    Api::with(
        Options::default()
            .set("RATE_LIMIT_IP_REQUESTS", ip)
            .set("RATE_LIMIT_USER_REQUESTS", user),
    )
    .await
}

#[tokio::test]
async fn platform_026_a_count_commits_though_its_request_fails_afterwards() {
    let api = Api::new().await;

    let response = users::sign_in(&api, "nobody@example.com", "wrong-password-123").await;

    assert_eq!(response.status, 401);
    let counts = api
        .database()
        .owner_reads("SELECT count::text FROM rate_limit_buckets ORDER BY count")
        .await;
    assert_eq!(counts, ["1", "1"], "the IP bucket and the email's");
}

#[tokio::test]
async fn platform_047_behind_a_trusted_proxy_the_client_ip_comes_from_x_forwarded_for() {
    let api = Api::with(
        Options::default()
            .set("TRUSTED_PROXIES", "198.51.100.0/24, 192.0.2.0/24")
            .set("RATE_LIMIT_IP_REQUESTS", 1),
    )
    .await;

    let first = api
        .get("/v1/capabilities")
        .header("x-forwarded-for", "203.0.113.5, 192.0.2.10")
        .send()
        .await;
    let same_client = api
        .get("/v1/capabilities")
        .from("198.51.100.99:4000")
        .header("x-forwarded-for", "10.9.9.9, 203.0.113.5")
        .send()
        .await;
    let other_client = api
        .get("/v1/capabilities")
        .header("x-forwarded-for", "203.0.113.6")
        .send()
        .await;

    assert_eq!(first.status, 200);
    assert_eq!(
        same_client.status, 429,
        "the same client, through another proxy"
    );
    assert_eq!(other_client.status, 200);
}

#[tokio::test]
async fn platform_047_an_untrusted_peer_is_the_client_whatever_it_forwards() {
    let api = limited(1, 1_000).await;

    let first = api
        .get("/v1/capabilities")
        .header("x-forwarded-for", "203.0.113.5")
        .send()
        .await;
    let spoofed = api
        .get("/v1/capabilities")
        .header("x-forwarded-for", "203.0.113.6")
        .send()
        .await;

    assert_eq!(first.status, 200);
    assert_eq!(spoofed.status, 429);
}

#[tokio::test]
async fn platform_048_a_client_ip_beyond_its_limit_answers_429() {
    let api = limited(2, 1_000).await;

    let statuses = [
        api.get("/v1/capabilities").send().await.status,
        api.get("/v1/nowhere").send().await.status,
        api.get("/v1/capabilities").send().await.status,
        api.get("/v1/capabilities")
            .from("192.0.2.1:1")
            .send()
            .await
            .status,
        api.get("/health/live").send().await.status,
    ];

    assert_eq!(
        statuses.map(|status| status.as_u16()),
        [200, 404, 429, 200, 200]
    );
}

#[tokio::test]
async fn platform_048_a_user_beyond_their_limit_answers_429() {
    let api = limited(1_000, 2).await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;

    let mut statuses = Vec::new();
    for _ in 0..3 {
        statuses.push(api.get("/v1/users/me").bearer(&ada.access).send().await);
    }
    let other = api.get("/v1/users/me").bearer(&grace.access).send().await;

    assert_eq!(statuses[1].status, 200);
    assert_eq!(statuses[2].status, 429);
    assert_eq!(statuses[2].json()["code"], "request.rate_limited");
    assert_eq!(other.status, 200);
}

#[tokio::test]
async fn platform_048_the_window_is_a_setting() {
    let api = Api::with(
        Options::default()
            .set("RATE_LIMIT_IP_REQUESTS", 10)
            .set("RATE_LIMIT_WINDOW_SECONDS", 30),
    )
    .await;

    let response = api.get("/v1/capabilities").send().await;

    assert_eq!(response.header("ratelimit-policy"), "\"ip\";q=10;w=30");
}

#[tokio::test]
async fn platform_019_the_probes_bypass_the_rate_limits() {
    let api = limited(1, 1_000).await;
    api.get("/v1/capabilities").send().await;

    let live = api.get("/health/live").send().await;
    let ready = api.get("/health/ready").send().await;

    assert_eq!(live.status, 200);
    assert_eq!(ready.status, 200);
    assert!(live.headers.get("ratelimit").is_none());
    assert_eq!(
        api.database()
            .owner_reads("SELECT count::text FROM rate_limit_buckets")
            .await,
        ["1"],
        "no probe counted"
    );
}

#[tokio::test]
async fn platform_049_a_window_restarts_with_the_first_request_after_it_ends() {
    let api = limited(1, 1_000).await;
    api.get("/v1/capabilities").send().await;
    let refused = api.get("/v1/capabilities").send().await;
    api.database()
        .owner_executes(
            "UPDATE rate_limit_buckets SET window_started_at = now() - interval '61 seconds'",
        )
        .await;

    let after = api.get("/v1/capabilities").send().await;

    assert_eq!(refused.status, 429);
    assert_eq!(after.status, 200);
    assert_eq!(
        api.database()
            .owner_reads("SELECT count::text FROM rate_limit_buckets")
            .await,
        ["1"]
    );
}

#[tokio::test]
async fn platform_049_a_refused_request_counts_too() {
    let api = limited(1, 1_000).await;

    for _ in 0..3 {
        api.get("/v1/capabilities").send().await;
    }

    assert_eq!(
        api.database()
            .owner_reads("SELECT count::text FROM rate_limit_buckets")
            .await,
        ["3"]
    );
}

#[tokio::test]
async fn platform_050_a_response_describes_the_bucket_with_the_fewest_requests_left() {
    let api = limited(10, 3).await;
    let ada = users::register(&api, "ada@example.com").await;

    let public = api.get("/v1/capabilities").send().await;
    let user = api.get("/v1/users/me").bearer(&ada.access).send().await;

    assert_eq!(public.header("ratelimit-policy"), "\"ip\";q=10;w=60");
    assert!(public.header("ratelimit").starts_with("\"ip\";r=8;t="));
    assert_eq!(user.header("ratelimit-policy"), "\"user\";q=3;w=60");
    assert!(user.header("ratelimit").starts_with("\"user\";r=2;t="));
}

#[tokio::test]
async fn platform_050_a_429_says_how_long_to_wait() {
    let api = limited(1, 1_000).await;
    api.get("/v1/capabilities").send().await;

    let refused = api.get("/v1/capabilities").send().await;

    assert_eq!(refused.status, 429);
    let wait: u64 = refused.header("retry-after").parse().unwrap();
    assert!((1..=60).contains(&wait), "{wait}");
    assert!(refused.header("ratelimit").starts_with("\"ip\";r=0;t="));
}

#[tokio::test]
async fn platform_051_no_bucket_holds_an_ip_address_an_email_or_a_user_id() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    api.get("/v1/users/me").bearer(&ada.access).send().await;
    users::sign_in(&api, "ada@example.com", "wrong-password-123").await;

    let keys = api
        .database()
        .owner_reads(
            "SELECT length(key_hash)::text || ' ' || encode(key_hash, 'escape') \
             FROM rate_limit_buckets",
        )
        .await;
    assert_eq!(keys.len(), 3, "the IP's, the user's and the email's");
    for key in keys {
        assert!(key.starts_with("32 "), "{key}");
        assert!(!key.contains("198.51.100.20") && !key.contains("ada@example.com"));
        assert!(!key.contains(&ada.user_id));
    }
}
