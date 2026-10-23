//! The `system` module ([spec](../../../../docs/spec/system.md)).

use http::StatusCode;
use serde_json::json;

use crate::support::Api;

#[tokio::test]
async fn system_001_the_capabilities_list_each_feature_a_deployment_can_turn_off() {
    let api = Api::new();

    let response = api.get("/v1/capabilities").send().await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("content-type"), "application/json");
    // No feature can be turned off until `mail` is built, and no version is reported.
    assert_eq!(response.json(), json!({"features": {}}));
}
