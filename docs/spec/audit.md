# Spec: `audit`

## Purpose

`audit` records who did what. Every module writes entries through it, inside the unit of work of the change (D1, D6). It also implements the `SecurityEventSink` that records refused requests. Reading the logs, the action catalog, retention and anonymization arrive with the full module.

## Owns

| Table | Holds |
|---|---|
| `audit_entries` | `id`, `scope` (`user` or `workspace`), `workspace_id`, `actor_user_id`, `target_user_id`, `action`, `resource_type`, `resource_id`, `values` (IDs and non-personal fields only), `client_ip`, `user_agent`, `trace_id`, `created_at`. |

## Interface

- `record(uow, Entry)`: writes one entry in the given unit of work.
- `SecurityEventSink`, implemented for `platform::authz`.
- Each module declares its actions in its own `audit.rs` (ARCH-03), named `<resource>.<past-tense verb>`, such as `member.removed`.

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| AUDIT-001 | An entry commits or rolls back together with the change it records. | DATA-27, D1 | `audit_001_` |
| AUDIT-002 | An entry's actor and scope come from the unit of work's proof, never from the caller's input. | D6, DATA-22 | `audit_002_` |
| AUDIT-003 | An entry records the client IP, the user agent and the trace ID of its request. Its `values` hold only IDs and non-personal fields. | SEC-16, OBS-07 | `audit_003_` |
| AUDIT-004 | A refused workspace request records `access.denied`, with the route's permission, in the workspace's log. | D6, ACCESS-003 | `audit_004_` |
| AUDIT-005 | No request can change or delete an entry: `app` holds only `INSERT` and `SELECT` on `audit_entries`. | D6, DATA-19 | `audit_005_` |

## Design

### Recording a refusal

A refusal happens before the refused operation has written anything. The `SecurityEventSink` implementation records it in its own short transaction: a member's refusal in the workspace's log, and a non-member's in the caller's own log, so a workspace's log never reveals who probed it. A rank refusal inside an operation reaches the sink through `authz::refuse`, and the operation's own unit of work rolls back (PLATFORM-025).

## Open design questions

| Question | Settled by |
|---|---|
| AUDIT-005 gives `app` only `INSERT` and `SELECT` on `audit_entries`, but retention (D6) deletes entries and erasure (D31) anonymizes them, and every job runs as `app`. Which role runs those jobs, or which narrower grant lets them run? | the full `audit` module's spec |
