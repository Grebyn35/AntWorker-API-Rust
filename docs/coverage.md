# Coverage map

Every foundational area of the API, and where it is decided. This file answers "did we miss anything?" by checking against a fixed list instead of a fresh guess each time.

## Why a coverage map

This map works from established references down, so it covers every area, whether or not a problem has been seen there:

- **The Twelve-Factor App:** how a deployable service is built and run.
- **The Application Security Verification Standard (ASVS) of the Open Worldwide Application Security Project (OWASP):** the areas of application security. Level 2 is the target bar (D26).
- **Google's practice of site reliability engineering (SRE):** monitoring, release engineering, capacity, overload and recovery.
- **Engineering foundations:** the code-level areas every codebase has to decide.
- **Template concerns:** what a forkable skeleton needs that a normal product doesn't.

Every area maps to a decision (`D`), a rule in [docs/standards/](standards/README.md) (by its prefix, such as `CODE`), a section of [architecture.md](architecture.md), a module spec in `docs/spec/` (`SPEC`), an owner question (`Q`), or "out of scope". An area is added only when a concrete requirement appears, not when a review imagines one.

## Twelve-Factor App

| Factor | Covered by |
|---|---|
| I. One codebase, many deploys | architecture.md §3, D21 |
| II. Explicit dependencies | `Cargo.lock`, pinned toolchain, `cargo-deny` (D21) |
| III. Config in the environment | D9 |
| IV. Backing services as attached resources | D9, D20 |
| V. Separate build, release and run | D21 (build once, promote the digest) |
| VI. Stateless processes | D20 |
| VII. Port binding | architecture.md §9 (the `serve` subcommand binds one port) |
| VIII. Scale out through processes | D20 |
| IX. Fast startup, graceful shutdown | D13, D20 |
| X. Development and production parity | D19, D28 |
| XI. Logs as event streams | D7, D10, D19 |
| XII. Admin tasks as one-off processes | D22 (binary subcommands), D15, D30 |

## OWASP ASVS areas

| Area | Covered by |
|---|---|
| Architecture and threat model | D22, D26 |
| Authentication | D18, D26, D61, D63, the `auth` spec |
| Session management | D5, D18, D61 |
| Access control | D2, D3, D4, D62 |
| Validation, sanitization and encoding | D25, D8 |
| Cryptography and stored data protection | D14, D26 |
| Error handling and logging | D17, D7 |
| Data protection and privacy | D6, D7, D14, D31, D40 |
| Secure communication | D26 (TLS and HTTP Strict Transport Security (HSTS) at Caddy) |
| Supply chain and malicious code | D21, D26 |
| Business logic and anti-automation | D11, the spec of each module |
| Files and outbound requests | D26 on server-side request forgery (SSRF), the `files` spec |
| API and web services | D8, D12 |
| Configuration and secrets | D9, D26 |

## Operations

| Area | Covered by |
|---|---|
| Monitoring, alerting, tracing | D10, D19, D42 |
| Release engineering | D15, D21, D45 |
| Capacity, overload and load shedding | D20 |
| Background work | D13 |
| Schema evolution | D15, D24 |
| Backups and disaster recovery | D30, D44 |
| Runbooks | D19 (one per alert), D30 |

## Engineering foundations

| Area | Covered by |
|---|---|
| Module structure and dependency rules | D22 |
| Error model | D17 |
| Domain types and input validation | D25 |
| Data access: IDs, time, queries, transactions, concurrency | D24 |
| Side effects and consistency | D1 |
| Testing strategy | D23 |
| API contract | D8 |
| Naming, formatting, comments, file and function size | CODE, DOC |
| Dependency policy, Rust edition and toolchain | CODE-01, CODE-11 to CODE-13, D21 |

## Template concerns

| Area | Covered by |
|---|---|
| Local development experience | D28 |
| Fork, rename, add and remove modules, workspace shapes, upstream updates | D29, D39, D62, D65, D66 |
| Documentation set and Claude Code guidance | D27 |
| Template versioning and changelog | D15, D29 |
| License | `Q1` |

## Product domains

Each module in [architecture.md §5](architecture.md#5-modules) gets a spec in `docs/spec/` before it is built. [The spec index](spec/README.md#specs) lists the specs that exist.

## Out of scope

Each of these gets a one-line architecture decision record (ADR) so the boundary is on record:

- Clients, and how they are distributed, such as a desktop app's auto-updater. Each client is a separate project that uses the API's contract.
- Kubernetes, Amazon Elastic Container Service (ECS) or Fly manifests (D20 documents the requirements).
- An admin console, billing, and multi-region deployment. The API's part in administering a deployment is the `superadmins` module (D62), which a client may build a console on.
- Localized server messages. Clients localize by error code and event type.

## Owner questions

| ID | Question |
|---|---|
| Q1 | Which license does the template ship under? Deferred: it blocks the first public release, not the build. Until then the repository stays private, and `cargo-deny` allows only permissive dependency licenses (CODE-12), so selling the template and open-sourcing it both stay possible. |
