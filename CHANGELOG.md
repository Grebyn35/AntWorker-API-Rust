# Changelog

What each release changes, for the forks that port it. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the versions follow semantic versioning, as [docs/operations/release.md](docs/operations/release.md) says how a release takes its version.

Each final release has a section here, under its version and its date, which `just release` asks for before it releases the version ([Before you release](docs/operations/release.md#before-you-release)). Beside what the release added, changed, removed and fixed, the section holds:

- **Security:** each fix of a vulnerability, marked as one, so a fork ports it first ([SECURITY.md](SECURITY.md)).
- **Upgrade notes:** what a fork does to take the release: the migrations it adds, the settings it adds, renames or removes, and what a deployment runs by hand, if anything.
- **Capacity:** the lines that `just load-test` and `just load-test streams` end with, measured before the release (OPS-16).

A prerelease has no section: what it holds stays under Unreleased until the final release of its number.

## [Unreleased]

### Added

- The template: the API, its reference deployment and the tasks that build, check and release them, as [docs/architecture.md](docs/architecture.md) describes them.

### Upgrade notes

None: no final release came before this one.
