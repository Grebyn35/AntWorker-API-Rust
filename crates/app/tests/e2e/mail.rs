//! Email from the deployed image (the platform spec's Mail), through a mail server on the tests'
//! own server.

use serde_json::json;

use crate::{deployment::Deployment, docker::lossy};

#[tokio::test]
async fn platform_066_the_deployed_api_sends_an_email_through_its_mail_server() {
    let deployment = Deployment::managed(&[]);
    let mail = deployment.server.start_mail_server();
    let mail: Vec<(&str, &str)> = mail
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    deployment.set(&mail);
    let applied = deployment.apply(&deployment.api, &[]).await;
    assert!(applied.status.success(), "{}", lossy(&applied));
    // The job that a module's `Mail::send` enqueues, as it holds the rendered email.
    let payload = json!({
        "message_id": "<deployed@example.com>",
        "email": {
            "to": "bob@example.com",
            "subject": "Sent by the deployment",
            "text": "The plain-text part.",
            "html": "<p>The HTML part.</p>",
        },
    });

    deployment.sql(&format!(
        "INSERT INTO jobs (kind, payload) VALUES ('mail.send', '{payload}')"
    ));

    // The email reaches the mail server once a replica's job sends it, within about a second.
    deployment.server.shell_ok(
        "timeout 60 sh -c 'until wget -q -O - \
         \"http://127.0.0.1:8025/api/v1/search?query=to%3Abob%40example.com\" 2> /dev/null \
         | grep -q \"Sent by the deployment\"; do sleep 1; done'",
    );
}
