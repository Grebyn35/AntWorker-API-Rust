//! The part of `check-metrics` that reads the dashboards as a whole (OBS-13): Grafana provisions
//! them read-only, and one overview links to every other dashboard.

use crate::{
    Finding,
    repo::Repo,
    stack::{self, Dashboards},
};

/// The file that tells Grafana where the dashboards are, and whether its users may change them.
const PROVIDERS: &str = "deploy/observability/grafana/provisioning/dashboards/dashboards.yaml";

/// Fails a provider that lets Grafana's users save a dashboard, a dashboard that is editable,
/// and dashboards that no single dashboard links to.
pub fn check(repo: &Repo, dashboards: &Dashboards, findings: &mut Vec<Finding>) {
    if let Some(providers) = stack::yaml(repo, PROVIDERS, findings) {
        let providers = providers["providers"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if providers.is_empty() {
            findings.push(Finding::new(
                PROVIDERS,
                1,
                "no provider provisions the dashboards (OBS-13)",
            ));
        }
        for provider in providers {
            if provider["allowUiUpdates"] != false {
                findings.push(Finding::new(
                    PROVIDERS,
                    1,
                    "a provider lets Grafana's users save its dashboards; set allowUiUpdates to false (OBS-13)",
                ));
            }
        }
    }
    for board in &dashboards.boards {
        if board.editable {
            findings.push(Finding::new(
                &board.path,
                1,
                "the dashboard is editable in Grafana; set editable to false (OBS-13)",
            ));
        }
    }
    let links_all = |overview: &stack::Board| {
        dashboards
            .boards
            .iter()
            .filter(|board| board.path != overview.path)
            .all(|board| {
                let target = format!("/d/{}", board.uid);
                overview.links.iter().any(|link| {
                    link == &target
                        || link.starts_with(&format!("{target}/"))
                        || link.starts_with(&format!("{target}?"))
                })
            })
    };
    if dashboards.boards.len() > 1 && !dashboards.boards.iter().any(links_all) {
        findings.push(Finding::new(
            stack::DASHBOARDS,
            1,
            "no overview dashboard links to every other dashboard (OBS-13)",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROVIDED: &str = "providers:\n  - name: dashboards\n    allowUiUpdates: false\n";

    fn board(uid: &str, editable: bool, links: &[&str]) -> String {
        let links: Vec<_> = links
            .iter()
            .map(|uid| serde_json::json!({"url": format!("/d/{uid}")}))
            .collect();
        serde_json::json!({"uid": uid, "editable": editable, "links": links, "panels": []})
            .to_string()
    }

    fn findings(providers: &str, boards: &[(&str, String)]) -> Vec<String> {
        let mut files = vec![(PROVIDERS, providers.to_owned())];
        let paths: Vec<String> = boards
            .iter()
            .map(|(name, _)| format!("{}{name}.json", stack::DASHBOARDS))
            .collect();
        for ((_, text), path) in boards.iter().zip(&paths) {
            files.push((path.as_str(), text.clone()));
        }
        let repo = Repo::of(
            &files
                .iter()
                .map(|(path, text)| (*path, text.as_str()))
                .collect::<Vec<_>>(),
        );
        let mut found = Vec::new();
        let dashboards = stack::dashboards(&repo, &mut found);
        check(&repo, &dashboards, &mut found);
        found.into_iter().map(|finding| finding.message).collect()
    }

    #[test]
    fn obs_13_the_dashboards_are_read_only_and_an_overview_links_to_each() {
        let read_only = findings(
            PROVIDED,
            &[
                (
                    "overview",
                    board("overview", false, &["requests", "platform"]),
                ),
                ("requests", board("requests", false, &["overview"])),
                ("platform", board("platform", false, &[])),
            ],
        );
        assert!(read_only.is_empty(), "{read_only:?}");
    }

    #[test]
    fn obs_13_a_dashboard_users_can_change_fails() {
        let saved = findings(
            "providers:\n  - name: dashboards\n",
            &[("overview", board("overview", true, &[]))],
        );
        assert_eq!(saved.len(), 2, "{saved:?}");
    }

    #[test]
    fn obs_13_a_dashboard_no_overview_reaches_fails() {
        let unlinked = findings(
            PROVIDED,
            &[
                ("overview", board("overview", false, &["requests"])),
                ("requests", board("requests", false, &["overview"])),
                ("platform", board("platform", false, &[])),
            ],
        );
        assert_eq!(unlinked.len(), 1, "{unlinked:?}");
    }
}
