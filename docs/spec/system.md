# Spec: `system`

## Purpose

`system` tells clients what the running deployment offers: the capabilities endpoint lists the features a deployment can turn off, and whether each one is on, and the optional parts that change what a client offers, which its fork keeps (D9, D118). It also fills a development database with demo accounts and workspaces, through the `seed` subcommand, so a developer has something to sign in to (D28). The schema endpoints, which list the permissions and the audit actions, arrive with the modules whose catalogs they list.

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
| SYSTEM-001 | `GET /v1/capabilities` answers 200 with a `features` object that holds one boolean per feature a deployment can turn off, `true` while the feature is on, and `true` for each optional part that changes what a client offers, under the part's name, which is absent once the fork removes the part; and `sign_in_providers`, the OAuth providers that a user signs in through, which AUTH-081 lists, in place of a member of `features` for each. | OPS-04, ARCH-07, D9, D118, AUTH-081 | `system_001_` |
| SYSTEM-002 | `seed` runs only when `ENVIRONMENT` is `development`. In any other environment it refuses before it connects to the database, so no deployment holds the demo accounts, whose password it prints. | D9, D28 | `system_002_` |
| SYSTEM-003 | `seed` creates its data through the API's routes, as a client that sets its own headers does, so every row it creates has passed the checks a client's request passes, and carries the audit entries and events that request records. It registers Olivia Owner, Adam Admin and Mia Member, at `owner@example.com`, `admin@example.com` and `member@example.com`, with one password. Olivia creates the workspace Acme. Mia creates the workspace Side Project. When it finishes, it prints each account's email and the password, and each workspace's name and ID, which the paths of its routes name. | D28, ARCH-14 | `system_003_` |
| SYSTEM-004 | When an account that `seed` registers exists already, it stops there and fails, saying that the database holds its data: it seeds a database once, and again only after the database is recreated. | D28, ERR-05 | `system_004_` |
| SYSTEM-005 | `seed` also registers Ian Invitee, at `invitee@example.com`, and in Acme has Olivia invite Adam as an admin and Mia as a member, who each accept, and Ian as a member, whose invitation stays pending. <!-- part: invitations --> | D28 | `system_005_` |
| SYSTEM-006 | With email on, `seed` proves each account's address as a client does, by confirming the code that its registration sent (AUTH-031), so its accounts sign in to a development deployment with email on and use it at once. With email off, nothing asks for a proof, and it confirms nothing. | D28, D67 | `system_006_` |
| SYSTEM-007 | `seed` builds the API with `SECOND_FACTOR_REQUIRED` off, whatever the setting, so its accounts, which have no authenticator app, create their workspaces through the routes; with the setting on, each sets an app up at its first sign-in, as an account that registered before the deployment turned the setting on does (AUTH-070). | D28, D138 | `system_007_` |

## Design

### Capabilities

- A feature is a member of `features`, so a release that adds a feature adds a field, which API-18 allows. A client tells a feature that is off, `false`, from one its API doesn't have, which is absent. A list of the features that are on couldn't tell those apart, and adding a value to a closed set of names would change the contract.
- `email` is the first feature a deployment can turn off: `true` while `EMAIL` is `smtp` (PLATFORM-022). A client hides what needs email while it is `false`, such as a way to reset a password.
- An optional part that changes what a client offers is a member of `features` too, under its part's name, `true` while the fork keeps it. `just remove` deletes the line that declares it with the part, whose marker it carries (ARCH-07), so a client tells a fork without the part from its API's being absent, as it does any feature's. A deployment can't turn a part off, so it is never `false`. So one client serves every fork, whatever parts it keeps, rather than a field that names the workspace shape, which couldn't state a fork that keeps `superadmins` in any shape, removes a part that no shape needs, or adds a part of its own. A part that changes nothing a client offers, such as the setup subcommand, which only an operator runs, has no member. The members tell what anyone could learn from the routes anyway, since a route that a fork removed answers 404 where one it kept asks for a credential.
- An OAuth provider is an optional part, but it is no member of `features`: a client needs its client IDs and its endpoints, not only whether the fork keeps it, so `sign_in_providers` lists it, once the deployment configures it, and the list holds only what anyone reads at the provider's own authorization page anyway ([auth spec](auth.md#signing-in-through-a-provider)). A fork without the provider lists it nowhere, as one that keeps it unconfigured doesn't, since no client offers a provider it can't use.
- The endpoint reports no build version, because the Application Security Verification Standard (ASVS) of the Open Worldwide Application Security Project (OWASP) asks that responses not reveal the versions of the API or of its components.

### Seed

`seed` is the binary's subcommand that fills a development database (D28), which `just seed` runs once the migrations are applied. The binary reads `serve`'s settings, refuses unless `ENVIRONMENT` is `development` (SYSTEM-002), connects to the database and composes the API as `serve` does, without binding a port or running a dispatcher, and hands the API's router to the seed. The seed changes the API's data only through its routes, as any client does: it sends each request in process, with a JSON body, the Bearer header of the account it acts as, the client IP 127.0.0.1 and the user agent `seed`, and reads the answer. The same pipeline handles each request: the IP limit counts it, its operation opens its unit of work as a request's does, as `app` (DATA-12), records its audit entries with that client IP, and publishes its events, so the seed runs no query of its own, and holds no knowledge of a module's tables that a change to them could leave behind. Beside the routes, it runs the jobs that its registrations enqueued, and reads the emails that they send to its inbox, as the next paragraph says, which a client does through the deployment's own jobs and its mailbox.

With email on, an account must prove its address before the API serves it anything else (AUTH-025), so the seed proves each one's as a client does, with the code its registration sent, which it reads as a client reads its mailbox. The binary builds the API's `Mail` with an inbox of its own, so the email that carries a code, which the job that makes the code sends at once (AUTH-030), goes there rather than to the mail server. Once it has registered its accounts, the seed runs the jobs their registrations enqueued, through `Jobs::run_pending`, since it runs no dispatcher, reads each account's code from its latest email, and confirms it through the route a client sends it to (AUTH-031). A code is never in a job's payload (SEC-03), so this inbox is the one place the seed can read it from, and only the seed may build one, as clippy enforces.

A deployment that requires a second factor refuses every request of an account without an app but those that setting one up needs (AUTH-070), so the binary builds the seed's API with the requirement off, whatever the setting (SYSTEM-007, D138): the seed creates its data as a client of a deployment without the requirement does, and a developer who signs in as one of its accounts on a deployment with it on sets the account's app up first, as the account's answers say.

A step that needs an optional part exists only for that part, and carries its marker, so removing the part removes the step (ARCH-07).
The invitations' emails wait in the job queue until `just dev` runs the API, whose jobs send them to the mail catcher (PLATFORM-023). <!-- part: invitations -->

Every account has the same password, which the seed prints with the accounts, since a developer signs in as each of them in turn. The accounts' addresses are at `example.com`, which no one can receive mail for (RFC 2606). A second run stops at the first account, which registering answers 409 (SYSTEM-004): the seed never signs in to an account it didn't create, and a database is seeded again by recreating it, as the README says.

## Open design questions

None.
