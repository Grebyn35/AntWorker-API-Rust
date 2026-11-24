# Spec: `system`

## Purpose

`system` tells clients what the running deployment offers: the capabilities endpoint lists the features a deployment can turn off, and whether each one is on, and the parts of the template that change what a client offers, which its fork keeps (D9, D118). It also fills a development database with demo accounts and workspaces, through the `seed` subcommand, so a developer has something to sign in to (D28). The capabilities list no event, no audit action and no route's permission: a client learns those from `openapi.json` (D170, D171).

## Owns

None.

## Interface

None. No other module uses `system`.

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `GET /v1/capabilities` | `public` | SYSTEM-001 |

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| SYSTEM-001 | `GET /v1/capabilities` answers 200 with a `features` object that holds one boolean per feature a deployment can turn off, `true` while the feature is on, and `true` for each part of the template that changes what a client offers, which the fork keeps and no deployment turns off, under the part's name; a member that exists for such a part is absent from a fork that leaves the part out; and `sign_in_providers`, the OAuth providers that a user signs in through, which AUTH-081 lists, in place of a member of `features` for each. | OPS-04, D9, D118, AUTH-081 | `system_001_` |
| SYSTEM-002 | `seed` runs only when `ENVIRONMENT` is `development`. In any other environment it refuses before it connects to the database, so no deployment holds the demo accounts, whose password it prints. | D9, D28 | `system_002_` |
| SYSTEM-003 | `seed` sends its requests to the API's routes, as a client that sets its own headers does, so every row they create has passed the checks a client's request passes, and carries the audit entries and events that request records. It registers Olivia Owner and Mia Member, at `owner@example.com` and `member@example.com`, with one password. When it finishes, it prints each account's email and the password, and the name and ID of each workspace it created, which the paths of its routes name. | D28, ARCH-14 | `system_003_` |
| SYSTEM-004 | When an account that `seed` registers exists already, it stops there and fails, saying that the database holds its data: it seeds a database once, and again only after the database is recreated. | D28, ERR-05 | `system_004_` |
| SYSTEM-005 | `seed` also registers Adam Admin and Ian Invitee, at `admin@example.com` and `invitee@example.com`, and in the first workspace it created, which Olivia owns, has Olivia invite Adam as an admin and Mia as a member, who each accept, and Ian as a member, whose invitation stays pending. <!-- part: invitations --> | D28 | `system_005_` |
| SYSTEM-006 | With email on, `seed` proves each account's address as a client does, by confirming the code that its registration sent (AUTH-031), so its accounts sign in to a development deployment with email on and use it at once. With email off, nothing asks for a proof, and it confirms nothing. | D28, D67 | `system_006_` |
| SYSTEM-007 | `seed` builds the API with `SECOND_FACTOR_REQUIRED` off, whatever the setting, so its accounts, which register without an authenticator app, are served the routes it sends as them; with the setting on, each of them that has no app sets one up at its first sign-in, as an account that registered before the deployment turned the setting on does (AUTH-070). | D28, D138 | `system_007_` |
| SYSTEM-008 | `seed` has Olivia create the workspace Acme, and Mia the workspace Side Project, through `POST /v1/workspaces`, before it creates any other. <!-- part: workspace_creation --> | D28 | `system_008_` |
| SYSTEM-009 | `seed` sets up the workspace Acme Labs for Olivia, by her address, once she has registered and, with email on, proved it, as an operator's run of `setup-workspace` does (WORKSPACES-023): through the operation that the subcommand runs, since no route sets a workspace up. With email on, the email that tells her waits in the job queue, as WORKSPACES-025 enqueues it. <!-- part: workspace_setup --> | D28, D152 | `system_009_` |
| SYSTEM-010 | `seed` also registers Sam Superadmin, at `superadmin@example.com`, and makes the account a superadmin, as an operator's run of `superadmin add` does (SUPERADMINS-001): through the operation that the subcommand runs, since no route makes one. Sam then sets up an authenticator app through the routes that a client sets one up through (AUTH-055, AUTH-056), and creates the workspace Acme Holdings through `POST /v1/superadmin/workspaces` (WORKSPACES-031). `seed` prints the app's secret beside the accounts, since a sign-in as Sam takes the app's code. <!-- part: superadmins --> | D28, D160 | `system_010_` |

## Design

### Capabilities

- A feature is a member of `features`, so a release that adds a feature adds a field, which API-18 allows. A client tells a feature that is off, `false`, from one its API doesn't have, which is absent. A list of the features that are on couldn't tell those apart, and adding a value to a closed set of names would change the contract.
- `email` is the first feature a deployment can turn off: `true` while `EMAIL` is `smtp` (PLATFORM-022). A client hides what needs email while it is `false`, such as a way to reset a password.
- A part of the template that changes what a client offers is a member of `features` too, under its part's name, `true` while the fork keeps it. A fork that leaves the part out has no such member, so a client tells a fork without the part from its member's being absent, as it does any feature's. A deployment can't turn such a part off, so its member is never `false`; a part whose feature a deployment can turn off has that feature's member instead, as `email` is one, which is absent in a fork that leaves the part out too.

  So one client serves every fork, whatever parts it keeps, rather than a field that names the workspace shape, which couldn't state a fork that leaves out a part that no shape needs, or adds a part of its own. A part that changes nothing a client offers has no member. The members tell what anyone could learn from the routes anyway, since a route that a fork left out answers 404 where one it kept asks for a credential.
  Users creating workspaces are the member `workspace_creation`: a client offers a user to create a workspace only where it is present, and otherwise tells a user who belongs to none that someone else gives them theirs. <!-- part: workspace_creation -->
  The setup subcommand is a part without a member, since only an operator runs it. <!-- part: workspace_setup -->
  The `invitations` module is the member `invitations`: a client offers a workspace's members to invite others only where it is present. <!-- part: invitations -->
  The `files` module is the member `files`: a client offers its user to upload files, an avatar and a workspace's picture only where it is present. <!-- part: files -->
  The `superadmins` module is the member `superadmins`: a client offers a superadmin's pages only where it is present, and only to a caller who reads that they are a superadmin (SUPERADMINS-006). <!-- part: superadmins -->
  The telemetry proxy is such a feature, under the name of its part, `client_telemetry`: the member is `true` while `CLIENT_TELEMETRY_ENDPOINT` names the collector's receiver, and a client asks its user whether it may send their telemetry, and sends it, only while it is `true` (CLIENT_TELEMETRY-010). <!-- part: client_telemetry -->
- An OAuth provider is a part of the template too, but it is no member of `features`: a client needs its client IDs and its endpoints, not only whether the fork keeps it, so `sign_in_providers` lists it, once the deployment configures it, and the list holds only what anyone reads at the provider's own authorization page anyway ([auth spec](auth.md#signing-in-through-a-provider)). A fork without the provider lists it nowhere, as one that keeps it unconfigured doesn't, since no client offers a provider it can't use.
- The endpoint reports no build version, because the Application Security Verification Standard (ASVS) of the Open Worldwide Application Security Project (OWASP) asks that responses not reveal the versions of the API or of its components.

### Seed

`seed` is the binary's subcommand that fills a development database (D28), which `just seed` runs once the migrations are applied. The binary reads `serve`'s settings, refuses unless `ENVIRONMENT` is `development` (SYSTEM-002), connects to the database and composes the API as `serve` does, without binding a port or running a dispatcher, and hands the API's router to the seed. The seed changes the API's data through its routes, as any client does: it sends each request in process, with its body, when it has one, as JSON, the Bearer header of the account it acts as, the client IP 127.0.0.1 and the user agent `seed`, and reads the answer. The same pipeline handles each request: the IP limit counts it, its operation opens its unit of work as a request's does, as `app` (DATA-12), records its audit entries with that client IP, and publishes its events, so the seed runs no query of its own, and holds no knowledge of a module's tables that a change to them could leave behind. Beside the routes, it runs the jobs that its registrations enqueued, and reads the emails that they send to its inbox, as the next paragraph says, which a client does through the deployment's own jobs and its mailbox.

Every product's data lives in a workspace, so a seed that created none would leave accounts that can do nothing. Each part that creates workspaces so adds its own step to the seed, in the way that a deployment of the fork creates them, and its first workspace is Olivia's: in every workspace shape, the first workspace that the seed created is hers, and what a later step does in a workspace, it does there.

<!-- part: workspace_setup -->
A workspace that an operator sets up has no route, so the seed plays the operator for that one step, beside its requests (SYSTEM-009, D152): the composition root hands it the operation that `setup-workspace` runs, since `system` uses no other module ([architecture §5](../architecture.md#dependencies)), and the seed gives it the workspace's name and Olivia's address, as the operator's arguments. The operation opens its own unit of work, as the subcommand's run does (WORKSPACES-023), on the seed's database, and enqueues its email through the seed's `Mail`.
<!-- end part: workspace_setup -->

<!-- part: superadmins -->
A superadmin creates a workspace through a route, which serves only an account that an operator made a superadmin, and that has an authenticator app (SUPERADMINS-005). So the seed plays the operator for that one step, and the superadmin's client for the rest (SYSTEM-010, D160): the composition root hands it the operation that `superadmin add` runs, since `system` uses no other module ([architecture §5](../architecture.md#dependencies)), which it gives Sam's address, once Sam has registered and, with email on, proved it. Sam then sets an app up as a client does, and the seed computes the code that confirms the setup, as an authenticator app does, from the secret that the setup answers (AUTH-057). An app's code depends on the time, by the database's clock, and the seed reads no clock (CODE-06): Sam re-authenticates first, and the answer's `authenticated_at` is the request's time by that clock (AUTH-021); it holds the session's new tokens, which Sam's later requests carry (AUTH-095). The seed prints the secret, in base32, which a developer adds to an authenticator app to sign in as Sam, and no recovery code: an account that lost its app gets it removed by the operator's subcommand, in development as on a deployment (AUTH-072).
Sam then hands Acme Holdings over to Olivia, as a superadmin hands over a workspace that they created: Sam invites her as an admin, she accepts, and Sam transfers the workspace to her, and leaves it (WORKSPACES-015, WORKSPACES-014). So in a fork where a superadmin's creation is the only one, the first workspace that the seed created is Olivia's too, and Sam, as a superadmin whose work in it is done, belongs to no workspace. <!-- part: invitations -->
<!-- end part: superadmins -->

With email on, an account must prove its address before the API serves it anything else (AUTH-025), so the seed proves each one's as a client does, with the code its registration sent, which it reads as a client reads its mailbox. The binary builds the API's `Mail` with an inbox of its own, so the email that carries a code, which the job that makes the code sends at once (AUTH-030), goes there rather than to the mail server. Once it has registered its accounts, the seed runs the jobs their registrations enqueued, through `Jobs::run_pending`, since it runs no dispatcher, reads each account's code from its latest email, and confirms it through the route a client sends it to (AUTH-031). A code is never in a job's payload (SEC-03), so this inbox is the one place the seed can read it from, and only the seed may build one, as clippy enforces.

A deployment that requires a second factor refuses every request of an account without an app but those that verifying an address or setting an app up needs (AUTH-070), so the binary builds the seed's API with the requirement off, whatever the setting (SYSTEM-007, D138): the seed creates its data as a client of a deployment without the requirement does, and a developer who signs in as one of its accounts on a deployment with it on sets the account's app up first, as the account's answers say.

A step that needs an optional part exists only for that part, and carries its marker, so removing the part removes the step (ARCH-07). <!-- part: removal -->
No step names what another part's step created, which a fork without that part doesn't have. <!-- part: removal -->
The invitations' emails wait in the job queue until `just dev` runs the API, whose jobs send them to the mail catcher (PLATFORM-023). <!-- part: invitations -->
The email that tells Olivia of the workspace set up for her waits in the job queue until `just dev` runs the API, whose jobs send it to the mail catcher (PLATFORM-023). <!-- part: workspace_setup -->
The email that tells Sam that an authenticator app now signs in to the account waits in the job queue until `just dev` runs the API, whose jobs send it to the mail catcher (AUTH-068, PLATFORM-023). <!-- part: superadmins -->

Every account has the same password, which the seed prints with the accounts, since a developer signs in as each of them in turn. The accounts' addresses are at `example.com`, which no one can receive mail for (RFC 2606). A second run stops at the first account, which registering answers 409 (SYSTEM-004): the seed never signs in to an account it didn't create, and a database is seeded again by recreating it, as the README says.

## Open design questions

None.
