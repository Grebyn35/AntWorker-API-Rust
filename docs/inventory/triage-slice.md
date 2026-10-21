# Triage: the first slice

Maps each old behavior the first slice touches to its verdict and to the spec behavior that carries it. Behaviors not listed here are triaged when their module is specified.

Verdicts:
- **kept:** the new behavior is the same.
- **changed:** the new behavior differs, for the reason given.
- **dropped:** no new behavior carries it.

| Old behavior | Verdict | New behavior | Reason for the change |
|---|---|---|---|
| AUTH-001 Register | changed | AUTH-001 | 201 Created (API-04); no workspace in the response (D3); audited in the same transaction |
| AUTH-002 Existing email on registration | changed | AUTH-002 | 409 `auth.email_in_use`; enumeration accepted on registration (D7) |
| AUTH-004 Log in | changed | AUTH-004 | No workspace resolution at sign-in (D3) |
| AUTH-005 Uniform login failures | changed | AUTH-005 | Failed attempts against an account are audited (D6) |
| AUTH-006 Per-email login rate limit | kept | AUTH-006 | |
| AUTH-014 Login lands in the default workspace | dropped | | No server-side active workspace (D3) |
| AUTH-015 Workspace resolution on refresh | dropped | | No server-side active workspace (D3) |
| AUTH-016 Opaque token pair, HMACs only | changed | AUTH-007 | Absolute session lifetime (D5); tokens stored as SHA-256 (D32) |
| AUTH-017 One session per device | changed | AUTH-008 | Device ID optional (D18); a replaced session is fully revoked (D5) |
| AUTH-019 Bearer validation | changed | AUTH-009 | Distinct code for an expired token; no session ID in the error (D8) |
| AUTH-022 Refresh rotation | changed | AUTH-010 | Sliding expiry bounded by the absolute lifetime (D5) |
| AUTH-023 Expired refresh token | kept | AUTH-011 | |
| AUTH-024 Refresh-token reuse | changed | AUTH-012 | Access-change signal, and audit in the same transaction (D1, D35) |
| AUTH-027 Log out | changed | AUTH-013 | 204 (API-04); the signal closes the session's streams (D35) |
| AUTH-055 Password hashing | changed | AUTH-014 | OWASP parameters; rehash on sign-in (D26) |
| USER-001 Read own profile | changed | ACCOUNTS-001 | Avatar and consent fields arrive with the full module |
| WS-001 New accounts start with no workspace | kept | AUTH-001 | |
| WS-002 Create a workspace | changed | WORKSPACES-001 | Path (D3), 201 (API-04), owner role (D2), workspace log (D6) |
| WS-003 Creating doesn't make the workspace active | dropped | | No server-side active workspace (D3) |
| WS-004 List my workspaces | changed | WORKSPACES-003 | Caller's role in each item; keyset paging; no total count (D8). The name filter arrives with the full module |
| WS-018 List members | changed | WORKSPACES-004 | Roles instead of permission lists (D2). Filters arrive with the full module |
| WS-023 Remove a member | changed | WORKSPACES-005 | Rank rule (D2), access-change signal (D35), audit in the same transaction (D1) |
| WS-024 Owner and caller can't be removed | changed | WORKSPACES-006 | The owner is protected by rank (SEC-08) |
| WS-028 Invite by email | changed | INVITATIONS-001, INVITATIONS-008 | A role instead of a permission list (D2); email sent by a job (D1). The invitee's live `invitation.received` is kept |
| WS-029 Existing members can't be invited | kept | INVITATIONS-002 | |
| WS-030 Re-inviting refreshes | changed | INVITATIONS-003 | Audited as an update; 200 |
| WS-034 List my pending invitations | changed | INVITATIONS-004 | Keyset paging instead of in-memory paging (API-06) |
| WS-035 Accept an invitation | changed | INVITATIONS-005 | Recorded in the workspace's log (D6); the membership and the invitation's deletion commit together (D34) |
| WS-036 Accept guards | changed | INVITATIONS-006 | Expiry is 422; an invitation for another email is 404 (DATA-22) |
| WS-038 Accepting while a member overwrites | changed | INVITATIONS-006 | 409, and nothing changes (D16) |
| WS-040 Invitation expiry | changed | INVITATIONS-007 | Hourly sweep. Managing expired invitations arrives with the full module |
| MAIL-028 Invitation email | changed | MAIL-002 | Sent by a job with retry (D1); not personalized with the invitee's account name |
| RT-001 Open the event stream | changed | REALTIME-001 | Filtered by permission (SEC-17); closed on loss of access (SEC-06) |
| RT-003 Event frame | kept | REALTIME-002 | |
| RT-004 Heartbeats | kept | REALTIME-003 | |
| RT-008 Replay after reconnect | changed | REALTIME-004 | Membership at the time of the event (SEC-17); truncation is signalled |
| AUDIT-010 Actor binding | kept | AUDIT-002 | |
| AUDIT-012 Client IP and user agent | changed | AUDIT-003 | Readable only with a dedicated permission (SEC-16); the IP comes only from trusted proxies (SEC-12) |
| AUDIT-013 Trace ID per entry | kept | AUDIT-003 | |

## Platform: the database

The old data layer's behaviors that `platform::db` replaces. The schema behaviors (DATA-019 to DATA-026) are triaged with the modules that own the tables, and the pool gauge (DATA-035) with the telemetry.

| Old behavior | Verdict | New behavior | Reason for the change |
|---|---|---|---|
| DATA-001 `migrate` applies the embedded migrations | kept | PLATFORM-027 | |
| DATA-002 Applied migrations are immutable | kept | PLATFORM-029 | Every applied migration is compared before anything is applied, so an edit can't hide behind an unapplied earlier version |
| DATA-003 Concurrent `migrate` runs apply each migration once | kept | PLATFORM-027 | |
| DATA-004 Versions the binary does not embed are ignored | kept | PLATFORM-029 | |
| DATA-005 A failing migration rolls back and fails the command | kept | PLATFORM-028 | |
| DATA-006 `migrate` reads only `DATABASE_URL` | changed | PLATFORM-027 | Reads `DATABASE_OWNER_URL` and the logging settings, and refuses any role but `owner` (DATA-12) |
| DATA-007 Schema changes run only in the one-shot | changed | PLATFORM-030 | `serve` also refuses to start against a schema that lacks one of its migrations |
| DATA-008 Runtime role bootstrap | changed | PLATFORM-010 | A subcommand instead of a script (ARCH-14); creates both roles; the password travels as a SCRAM verifier |
| DATA-009 Runtime role privileges | changed | PLATFORM-010, DATA-19 | Default privileges instead of grants by table name; no temporary tables; role-level timeouts |
| DATA-010 SECURITY DEFINER functions pin `search_path` | dropped | | No `SECURITY DEFINER` functions (DATA-07) |
| DATA-011 Per-transaction RLS context | dropped | | No row-level security (D4) |
| DATA-012 to DATA-018 RLS policies by table | changed | DATA-22, TEST-09 | Tenancy is enforced by each query's own filter, checked by `check-tenancy` and the foreign-workspace probes (D4) |
| DATA-027 Transaction helpers | changed | ARCH-11, PLATFORM-024 | One `UnitOfWork`, opened by the route and committed by the service |
| DATA-028 `_in_tx` methods re-activate context | dropped | | No row-level security (D4) |
| DATA-029 Database error classification and HTTP mapping | changed | ERR-04, ERR-05 | A repository maps each constraint it relies on, and every other database failure is `Internal`. The status for an unreachable database comes with the HTTP errors |
| DATA-030 Service-layer database retry | dropped | | Nothing inside the API retries a transaction: locks are taken in a consistent order, and a deadlock is a defect (DATA-25). Jobs keep their retry (OPS-10) |
| DATA-031 Background-task retry | changed | PLATFORM-013 | Jobs retry with backoff, then move to a dead-letter state (OPS-10) |
| DATA-034 Connection pool | changed | PLATFORM-009 | `DATABASE_MAX_CONNECTIONS`, 10 by default; a 5-second acquire timeout as a constant; no minimum connections |
| DATA-036 Repository access split | dropped | | No repositories that bypass row-level security (D4, D22) |
