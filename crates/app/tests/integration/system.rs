//! The `system` module ([spec](../../../../docs/spec/system.md)).

use http::StatusCode;
use serde_json::json;

use crate::support::{Api, Options};

#[tokio::test]
async fn system_001_the_capabilities_list_each_feature_a_deployment_can_turn_off() {
    let api = Api::new().await;

    let response = api.get("/v1/capabilities").send().await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("content-type"), "application/json");
    // Email is the one feature a deployment can turn off, and no version is reported.
    assert_eq!(response.json(), json!({"features": {"email": true}}));
}

#[tokio::test]
async fn system_001_a_feature_turned_off_is_false() {
    let api = Api::with(Options::default().set("EMAIL", "off")).await;

    let response = api.get("/v1/capabilities").send().await;

    assert_eq!(response.json(), json!({"features": {"email": false}}));
}
