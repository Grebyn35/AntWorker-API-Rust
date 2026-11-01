//! The emails that the Mailpit of `just services` received (MAIL-005), read through its API.
//! Every test runs against the same Mailpit, so a test sends only to addresses of its own, from
//! [`address`], and reads only those.

use std::net::SocketAddr;

use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

/// Where Mailpit's API listens.
const MAILPIT: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8025);

/// An email that Mailpit received.
#[derive(Clone, Debug)]
pub struct Email {
    /// Its subject.
    pub subject: String,
    /// The sender's address.
    pub from: String,
    /// The sender's name.
    pub from_name: String,
    /// Its `Message-ID`, without the angle brackets.
    pub message_id: String,
    /// Its plain-text part.
    pub text: String,
    /// Its HTML part.
    pub html: String,
}

/// An address that no other test sends to: `name`, a random tag, and `@example.com`.
pub fn address(name: &str) -> String {
    let tag: [u8; 6] = platform::random::bytes().unwrap();
    let tag: String = tag.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{name}-{tag}@example.com")
}

/// The emails sent to `address`, the newest first.
pub async fn to(address: &str) -> Vec<Email> {
    let query: String =
        url::form_urlencoded::byte_serialize(format!("to:\"{address}\"").as_bytes()).collect();
    let found = get(&format!("/api/v1/search?query={query}")).await;
    let mut emails = Vec::new();
    for message in found["messages"].as_array().unwrap() {
        let id = message["ID"].as_str().unwrap();
        let message = get(&format!("/api/v1/message/{id}")).await;
        emails.push(Email {
            subject: string(&message["Subject"]),
            from: string(&message["From"]["Address"]),
            from_name: string(&message["From"]["Name"]),
            message_id: string(&message["MessageID"]),
            text: string(&message["Text"]),
            html: string(&message["HTML"]),
        });
    }
    emails
}

fn string(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// The JSON that Mailpit's API answers to `GET path`. The request is HTTP/1.0, so the answer's
/// body comes whole, never in chunks, and ends with the connection.
async fn get(path: &str) -> Value {
    let mut stream = TcpStream::connect(MAILPIT)
        .await
        .expect("Mailpit, which `just services` starts");
    let request = format!("GET {path} HTTP/1.0\r\nHost: mailpit\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    assert!(
        head.starts_with("HTTP/1.0 200") || head.starts_with("HTTP/1.1 200"),
        "{head}"
    );
    serde_json::from_str(body).unwrap()
}
