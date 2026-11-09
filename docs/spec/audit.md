# Spec: `audit`

## Purpose

`audit` records who did what. Every module writes entries through it, inside the unit of work of the change (D1, D6). It also implements the `SecurityEventSink` that records refused requests. Deleting a user anonymizes the entries that name them (AUDIT-006). Reading the logs, the action catalog and retention arrive with the full module.

## Owns

| Table | Holds |
|---|---|
| `audit_entries` | `id`; `scope`, `user` or `workspace`; `user_id`, the user whose log holds an entry of the user scope; `workspace_id`, the workspace whose log holds one of the workspace scope; `actor_user_id`, `target_user_id`, `action`, `resource_type`, `resource_id`; `details`, IDs and non-personal fields only; `trace_id` and `created_at`. |
| `audit_entry_clients` | The client of an entry recorded in a request: `id`, `entry_id`, which is unique; `user_id`, the user whose deletion deletes it: the entry's actor, or, for an entry without one, the user whose log holds it; `client_ip`, `user_agent` and `created_at`. Both columns are personal data (DATA-23). |

## Interface

- `record(uow, scope, Entry)`: writes one entry in the given unit of work. The scope names the log and the actor, and comes only from a proof: `Principal::audit_scope`, the user's own log with the user as the actor, or that of `Authorized<P>`, the workspace's log with the member as the actor. `AuditScope::attempt(user_id)` names the log of an account that an unauthenticated request acted on, such as a failed sign-in, with no actor. `Principal::founder_scope(workspace_id)` names the log of the workspace the caller just created, with them as the actor, before any `Authorized<P>` can name it, and only the operation that creates a workspace may call it, which clippy enforces.
  `Principal::joiner_scope(workspace_id)` does the same for the workspace the caller joins by accepting an invitation (INVITATIONS-005), and only accepting may call it. <!-- part: invitations -->
  `Principal::leaver_scope(workspace_id)` names the log of a workspace that the caller leaves as their account is deleted, and only the personal data contract of `workspaces` may call it (ACCOUNTS-014).
- `SecurityEventSink`, implemented for `platform::authz`.
- `Contract`, the personal data contract for `audit_entries` and `audit_entry_clients` (D31), whose erasure the foreign keys carry out (AUDIT-006).
- `Action` and `Resource`, the types of an entry's action and of the kind of resource it happened to. Each module declares its actions, named `<resource>.<past-tense verb>`, such as `member.removed`, and its kinds of resource, such as `workspace`, as constants in its own `audit.rs` (ARCH-03): clippy bans their constructors everywhere else.

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| AUDIT-001 | An entry commits or rolls back together with the change it records. | DATA-22, D1 | `audit_001_` |
| AUDIT-002 | An entry's actor and scope come from the unit of work's proof, never from the caller's input. | D6, DATA-16 | `audit_002_` |
| AUDIT-003 | An entry records the client IP, the user agent and the trace ID of its request. Its `details` hold only IDs and non-personal fields. | SEC-18, OBS-07 | `audit_003_` |
| AUDIT-004 | A refused workspace request records `access.denied`, with the route's permission: a member's in the workspace's log, and a non-member's in their own, with the workspace as its resource; a non-member whose account was deleted while the refused request ran has no log left, and their refusal is recorded nowhere. | D6, ACCESS-003 | `audit_004_` |
| AUDIT-005 | No request can change or delete an entry: `app` holds only `INSERT` and `SELECT` on `audit_entries` and `audit_entry_clients`. Deleting a user changes entries through foreign keys alone (AUDIT-006). | D6, DATA-19 | `audit_005_` |
| AUDIT-006 | An entry names a user only as its actor, its target, or the user whose log holds it, and, in that user's own log alone, as its resource; never in its details. Deleting the user deletes their own log, and, in every other log, sets to null each entry's actor and target that was them, and deletes the client IP and user agent of each entry they acted in. | D6, D31, D124, SEC-18 | `audit_006_` |

## Design

### Recording an entry

`record` inserts one row through the unit of work's executor, so the entry commits with the change (AUDIT-001), and, in the same statement, the row of its client. The client IP and the user agent come from the unit of work, which opens with its request's (platform spec, "Unit of work"); a job's unit of work holds neither, and records no client. The trace ID is that of the span the entry is recorded in (OBS-07). An `Entry` holds the action, the resource's type and ID, the target user if any, and `details`, a JSON object a module builds from IDs and non-personal fields (SEC-18).

`AuditScope` is a `platform` type whose fields are private: only the proofs build one with an actor, so no module can record an entry as someone the request didn't prove it is (AUDIT-002). An entry of the user scope sets `user_id` and leaves `workspace_id` null; one of the workspace scope does the reverse, which the check `audit_entries_scope_check` enforces. `scope` is a Postgres enum (DATA-04), while `action` and `resource_type` are text: each module declares its own actions and resources in its `audit.rs`, so adding one changes no type that every module's entries share.

### Keys and grants

`user_id` references `users` with `ON DELETE CASCADE`, since a user's own log belongs to their account. `actor_user_id` and `target_user_id` reference `users` with `ON DELETE SET NULL`, so deleting a user anonymizes the entries of other logs that name them (D6, D31). `workspace_id` references `workspaces` with `ON DELETE CASCADE`, since a workspace's log belongs to the workspace, through a migration of its own that follows the one creating `workspaces`. The migration that creates the tables revokes `UPDATE` and `DELETE` on both from `app`, which the first migration's default privileges would otherwise grant (AUDIT-005).

An entry's client IP and user agent are its actor's, or, for an entry without one, such as a failed sign-in, are kept as long as the log of the user that holds it. They live in `audit_entry_clients`, whose `entry_id` references the entry with `ON DELETE CASCADE`, and whose `user_id` references that user with `ON DELETE CASCADE`, so deleting the user deletes the client of every entry that was theirs, while the entry stays. Postgres carries out a foreign key's action with the privileges of its table's owner, so the deletion of a user, which `app` may make, anonymizes the entries without `app` holding `UPDATE` or `DELETE` on either table, and no request can change an entry any other way (AUDIT-005). A column of the entry itself couldn't go so: a foreign key's action sets to null only its own columns.

An entry names a user only in the three columns that reference `users`, or, in that user's own log, which goes with them, as its resource, so these actions leave no entry that names a deleted user (AUDIT-006): an action on a member names the workspace as its resource, and the member as its target, such as `member.removed`, and an action whose resource is a user, such as `user.renamed`, is recorded only in that user's own log.

### Recording a refusal

A refusal happens before the refused operation has written anything. The `SecurityEventSink` implementation records it in its own short transaction: a member's refusal in the workspace's log, and a non-member's in the caller's own log, so a workspace's log never reveals who probed it. A rank refusal inside an operation reaches the sink through `authz::refuse`, and the operation's own unit of work rolls back (PLATFORM-010). A non-member's refusal whose own log went with their account while the refused request ran, such as a read that waited for its own caller's deletion, is recorded nowhere: the foreign key of the entry's log, `audit_entries_user_id_fkey`, refuses it, which the sink reads by its name (ERR-05), and the request still answers as refused (AUDIT-004).

## Open design questions

| Question | Settled by |
|---|---|
| AUDIT-005 gives `app` only `INSERT` and `SELECT` on `audit_entries`, but retention (D6) deletes old entries, and every job runs as `app`. Which role runs retention, or which narrower grant lets it run? | the full `audit` module's spec |
