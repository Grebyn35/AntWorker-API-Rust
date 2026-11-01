# Spec: `audit`

## Purpose

`audit` records who did what. Every module writes entries through it, inside the unit of work of the change (D1, D6). It also implements the `SecurityEventSink` that records refused requests. Reading the logs, the action catalog, retention and anonymization arrive with the full module.

## Owns

| Table | Holds |
|---|---|
| `audit_entries` | `id`; `scope`, `user` or `workspace`; `user_id`, the user whose log holds an entry of the user scope; `workspace_id`, the workspace whose log holds one of the workspace scope; `actor_user_id`, `target_user_id`, `action`, `resource_type`, `resource_id`; `details`, IDs and non-personal fields only; `client_ip`, `user_agent`, `trace_id` and `created_at`. |

## Interface

- `record(uow, scope, Entry)`: writes one entry in the given unit of work. The scope names the log and the actor, and comes only from a proof: `Principal::audit_scope`, the user's own log with the user as the actor, or that of `Authorized<P>`, the workspace's log with the member as the actor. `AuditScope::attempt(user_id)` names the log of an account that an unauthenticated request acted on, such as a failed sign-in, with no actor.
- `SecurityEventSink`, implemented for `platform::authz`.
- Each module declares its actions in its own `audit.rs` (ARCH-03), named `<resource>.<past-tense verb>`, such as `member.removed`.

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| AUDIT-001 | An entry commits or rolls back together with the change it records. | DATA-27, D1 | `audit_001_` |
| AUDIT-002 | An entry's actor and scope come from the unit of work's proof, never from the caller's input. | D6, DATA-22 | `audit_002_` |
| AUDIT-003 | An entry records the client IP, the user agent and the trace ID of its request. Its `details` hold only IDs and non-personal fields. | SEC-16, OBS-07 | `audit_003_` |
| AUDIT-004 | A refused workspace request records `access.denied`, with the route's permission, in the workspace's log. | D6, ACCESS-003 | `audit_004_` |
| AUDIT-005 | No request can change or delete an entry: `app` holds only `INSERT` and `SELECT` on `audit_entries`. | D6, DATA-19 | `audit_005_` |

## Design

### Recording an entry

`record` inserts one row through the unit of work's executor, so the entry commits with the change (AUDIT-001). The client IP and the user agent come from the unit of work, which a route opens with its request's (platform spec, "Unit of work"); a job's unit of work holds neither, and records them as null. The trace ID is that of the span the entry is recorded in (OBS-07). An `Entry` holds the action, the resource's type and ID, the target user if any, and `details`, a JSON object a module builds from IDs and non-personal fields (SEC-16).

`AuditScope` is a `platform` type whose fields are private: only the proofs build one with an actor, so no module can record an entry as someone the request didn't prove it is (AUDIT-002). An entry of the user scope sets `user_id` and leaves `workspace_id` null; one of the workspace scope does the reverse, which the check `audit_entries_scope_check` enforces. `scope` is a Postgres enum (DATA-04), while `action` and `resource_type` are text: each module declares its own actions and resources in its `audit.rs`, so adding one changes no type that every module's entries share.

### Keys and grants

`user_id` references `users` with `ON DELETE CASCADE`, since a user's own log belongs to their account. `actor_user_id` and `target_user_id` reference `users` with `ON DELETE RESTRICT`, until the full module's erasure anonymizes them (D31). `workspace_id` gains its key to `workspaces`, with `ON DELETE CASCADE`, when step 2.2 creates that table. The migration that creates the table revokes `UPDATE` and `DELETE` on it from `app`, which the first migration's default privileges would otherwise grant (AUDIT-005).

### Recording a refusal

A refusal happens before the refused operation has written anything. The `SecurityEventSink` implementation records it in its own short transaction: a member's refusal in the workspace's log, and a non-member's in the caller's own log, so a workspace's log never reveals who probed it. A rank refusal inside an operation reaches the sink through `authz::refuse`, and the operation's own unit of work rolls back (PLATFORM-025).

## Open design questions

| Question | Settled by |
|---|---|
| AUDIT-005 gives `app` only `INSERT` and `SELECT` on `audit_entries`, but retention (D6) deletes entries and erasure (D31) anonymizes them, and every job runs as `app`. Which role runs those jobs, or which narrower grant lets them run? | the full `audit` module's spec |
