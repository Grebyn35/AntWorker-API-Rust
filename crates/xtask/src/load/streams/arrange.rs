//! The state that the hub's load test arranges before it opens the streams (OPS-16): its users,
//! each the owner and only member of a workspace of their own, and the data k6 publishes with.

use std::{collections::BTreeMap, path::Path};

use serde_json::{Value, json};

use super::{USERS, client};
use crate::load::{compose_id, docker, replica::OWNER_URL};

/// The users whose streams the hub holds, each with an access token, and the data k6 publishes
/// with: each user's workspace, of which they are the owner and the only member, with their token.
/// Their addresses hold `run`. The workspaces are arranged in the database, as a test arranges
/// its state, rather than through one of the parts that create workspaces, so the test runs
/// whichever of them a fork keeps.
pub(super) fn arrange(
    root: &Path,
    settings: &[String],
    publisher: &str,
    run: &str,
) -> Result<Vec<String>, String> {
    let mut tokens = BTreeMap::new();
    for n in 0..USERS {
        let email = format!("load-{run}-{n}@example.com");
        let body =
            json!({ "email": email, "password": "plum-tractor-41-quiet", "name": "Load Test" });
        let token = match client::register(publisher, &body)? {
            (201, answer) => answer["access_token"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("registering {email} answered no access token"))?,
            (status, answer) => {
                return Err(format!("registering {email} answered {status}: {answer}"));
            }
        };
        tokens.insert(email, token);
    }
    let owned = owned_workspaces(root, settings, run)?;
    let mut workspaces = Vec::new();
    for (email, workspace) in owned {
        let token = tokens.get(&email).ok_or_else(|| {
            format!("{email} owns a workspace, though the task registered no such user")
        })?;
        workspaces.push(json!({ "id": workspace, "token": token }));
    }
    if workspaces.len() != USERS {
        return Err(format!(
            "{} of the {USERS} users own a workspace",
            workspaces.len()
        ));
    }
    let data = json!({ "workspaces": workspaces });
    let directory = root.join("target/load");
    write(&directory, &data).map_err(|error| format!("can't write the data k6 reads: {error}"))?;
    Ok(tokens.into_values().collect())
}

/// Arranges a workspace in the load test's database for each account whose address holds `run`,
/// owned by that account alone, as the role that owns the schema, whose URL the replica's
/// `settings` hold, and returns each account's address with its workspace's ID.
fn owned_workspaces(
    root: &Path,
    settings: &[String],
    run: &str,
) -> Result<Vec<(String, String)>, String> {
    let url = settings
        .iter()
        .find_map(|setting| setting.strip_prefix(OWNER_URL)?.strip_prefix('='))
        .ok_or_else(|| format!("the replica's settings hold no {OWNER_URL}"))?;
    // Each workspace takes its ID from the column default (DATA-08), and the n-th of them, in
    // the order of their IDs, goes to the n-th account, in the order of their addresses.
    let statement = format!(
        "WITH owners AS (SELECT id, email, row_number() OVER (ORDER BY email) AS n \
         FROM users WHERE email LIKE 'load-{run}-%'), \
         created AS (INSERT INTO workspaces (name) SELECT 'Load Test' FROM owners RETURNING id), \
         numbered AS (SELECT id, row_number() OVER (ORDER BY id) AS n FROM created), \
         joined AS (INSERT INTO workspace_memberships (workspace_id, user_id, role) \
         SELECT numbered.id, owners.id, 'owner' FROM numbered JOIN owners USING (n) \
         RETURNING workspace_id, user_id) \
         SELECT owners.email, joined.workspace_id FROM joined \
         JOIN owners ON owners.id = joined.user_id"
    );
    let rows = docker(
        root,
        &[
            "exec",
            &compose_id(root, "postgres"),
            "psql",
            url,
            "-qAt",
            "-v",
            "ON_ERROR_STOP=1",
            "--command",
            &statement,
        ],
    )
    .map_err(|error| format!("arranging the workspaces failed: {error}"))?;
    rows.lines()
        .map(|row| {
            row.split_once('|')
                .map(|(email, workspace)| (email.to_owned(), workspace.to_owned()))
                .ok_or_else(|| format!("arranging the workspaces answered {row:?}"))
        })
        .collect()
}

#[expect(
    clippy::disallowed_methods,
    reason = "the task runs as a synchronous program, with no async runtime to stall"
)]
fn write(directory: &Path, data: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    std::fs::write(directory.join("streams.json"), data.to_string())
}
