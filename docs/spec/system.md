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
| SYSTEM-001 | `GET /v1/capabilities` answers 200 with a `features` object that holds one boolean per feature a deployment can turn off, `true` while the feature is on, and `true` for each optional part that changes what a client offers, under the part's name, which is absent once the fork removes the part. | OPS-04, ARCH-07, D9, D118 | `system_001_` |
| SYSTEM-002 | `seed` runs only when `ENVIRONMENT` is `development`. In any other environment it refuses before it connects to the database, so no deployment holds the demo accounts, whose password it prints. | D9, D28 | `system_002_` |
| SYSTEM-003 | `seed` creates its data through the API's routes, as a client that sets its own headers does, so every row it creates has passed the checks a client's request passes, and carries the audit entries and events that request records. It registers Olivia Owner, Adam Admin and Mia Member, at `owner@example.com`, `admin@example.com` and `member@example.com`, with one password. Olivia creates the workspace Acme. Mia creates the workspace Side Project. When it finishes, it prints each account's email and the password, and each workspace's name and ID, which the paths of its routes name. | D28, ARCH-14 | `system_003_` |
| SYSTEM-004 | When an account that `seed` registers exists already, it stops there and fails, saying that the database holds its data: it seeds a database once, and again only after the database is recreated. | D28, ERR-05 | `system_004_` |
| SYSTEM-005 | `seed` also registers Ian Invitee, at `invitee@example.com`, and in Acme has Olivia invite Adam as an admin and Mia as a member, who each accept, and Ian as a member, whose invitation stays pending. <!-- part: invitations --> | D28 | `system_005_` |

## Design

### Capabilities

- A feature is a member of `features`, so a release that adds a feature adds a field, which API-18 allows. A client tells a feature that is off, `false`, from one its API doesn't have, which is absent. A list of the features that are on couldn't tell those apart, and adding a value to a closed set of names would change the contract.
- `email` is the first feature a deployment can turn off: `true` while `EMAIL` is `smtp` (PLATFORM-022). A client hides what needs email while it is `false`, such as a way to reset a password.
- An optional part that changes what a client offers is a member of `features` too, under its part's name, `true` while the fork keeps it. `just remove` deletes the line that declares it with the part, whose marker it carries (ARCH-07), so a client tells a fork without the part from its API's being absent, as it does any feature's. A deployment can't turn a part off, so it is never `false`. So one client serves every fork, whatever parts it keeps, rather than a field that names the workspace shape, which couldn't state a fork that keeps `superadmins` in any shape, removes a part that no shape needs, or adds a part of its own. A part that changes nothing a client offers, such as the setup subcommand, which only an operator runs, has no member. The members tell what anyone could learn from the routes anyway, since a route that a fork removed answers 404 where one it kept asks for a credential.
- The endpoint reports no build version, because the Application Security Verification Standard (ASVS) of the Open Worldwide Application Security Project (OWASP) asks that responses not reveal the versions of the API or of its components.

### Seed

`seed` is the binary's subcommand that fills a development database (D28), which `just seed` runs once the migrations are applied. The binary reads `serve`'s settings, refuses unless `ENVIRONMENT` is `development` (SYSTEM-002), connects to the database and composes the API as `serve` does, without binding a port or running the jobs, and hands the API's router to the seed. The seed knows the API only through its routes, as any client does: it sends each request in process, with a JSON body, the Bearer header of the account it acts as, the client IP 127.0.0.1 and the user agent `seed`, and reads the answer. The same pipeline handles each request: the IP limit counts it, its operation opens its unit of work as a request's does, as `app` (DATA-12), records its audit entries with that client IP, and publishes its events, so the seed needs no way into the database of its own, and holds no knowledge of a module's tables that a change to them could leave behind.

A step that needs an optional part exists only for that part, and carries its marker, so removing the part removes the step (ARCH-07).
The invitations' emails wait in the job queue until `just dev` runs the API, whose jobs send them to the mail catcher (PLATFORM-023). <!-- part: invitations -->

Every account has the same password, which the seed prints with the accounts, since a developer signs in as each of them in turn. The accounts' addresses are at `example.com`, which no one can receive mail for (RFC 2606). A second run stops at the first account, which registering answers 409 (SYSTEM-004): the seed never signs in to an account it didn't create, and a database is seeded again by recreating it, as the README says.

## Open design questions

None.
