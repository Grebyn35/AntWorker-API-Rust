# Adding a module

How to add a module that holds a product's data, how to add a route to a module, new or old, how to add a provider that users sign in through, and how to add a subcommand that an operator runs. It names where each piece goes and which rules it follows; the rules themselves are in [the standards](../standards/README.md), and how the parts fit together in [architecture.md](../architecture.md). An ID in parentheses names what a sentence follows: a rule of the [standards](../standards/README.md), such as ARCH-06, a behavior of a [spec](../spec/README.md), such as PLATFORM-005, or a decision, such as D125, whose records [architecture.md](../architecture.md) says where to find. The [glossary](../architecture.md#11-glossary) defines the terms these documents share.

## Module or route

A module owns tables and the one area of the product they serve ([architecture §5](../architecture.md#what-each-module-owns)). A route over data that a module already owns goes in that module, since no other module may query its tables (ARCH-06): skip to [Adding a route](#adding-a-route). Data a module adds for its product lives in a workspace (DATA-16), which is what `just new-module` writes.

## Writing the module

```
just new-module order_items              # the resource is order_item, the regular singular
just new-module people person            # a resource that the regular plural doesn't give
just new-module categories category      # a name whose ending two kinds of plural share, as movies' does
```

The module's name is plural and the resource's singular, both in `snake_case`. The task refuses a module that the repository has, or had, and its table's or permissions' name if they exist, and a resource whose type the module's code names already, such as `page`, whose `Page` is `platform::lists::Page`, or whose schema name a type of the code takes, such as `role`'s `Role`; it says why, and suggests naming the resource. `just new-module` writes a module that already follows every rule, with one resource: a row of a workspace with a `name`. Every role may create, list, read and rename the rows, and an owner or an admin delete them; each change commits with an entry in the workspace's audit log and an event to the members who may read the rows. A step that fails undoes what the task wrote.

The task needs Postgres, which it starts, and leaves the development database as it was. Stop `just dev` before you run it, and start it again once the migration holds the columns you need: `just dev` applies the new migrations whenever a `.rs`, `.sql` or `.toml` file under `crates/` or `migrations/` changes, and `migrate` refuses a migration whose file changed after it was applied, until the database is recreated, as [Migrations and the query cache](#migrations-and-the-query-cache) says.

It writes these files, each holding what [architecture §5](../architecture.md#inside-a-module) says its name holds, where `<module>` is the module's name, such as `order_items`:

| File | Holds |
|---|---|
| `migrations/<timestamp>_<module>_<module>.sql` | The module's table, its index for the list, and its column comments |
| `crates/app/src/modules/<module>/` | `mod.rs`, `routes.rs`, `service.rs`, `repo.rs`, `model.rs`, `error.rs`, `events.rs`, `audit.rs`, and `personal_data.rs`, whose contract covers the `name` its migration comments as personal data, exports the rows of each workspace whose only member is the user (D40), and leaves a deleted member's rows to their workspace (DATA-23, D125) |
| `crates/app/tests/integration/<module>.rs` | A test for each behavior |
| `crates/app/tests/integration/support/<module>.rs` | The builder that arranges the module's rows for tests (TEST-09) |
| `docs/spec/<module>.md` | The spec: its routes, and a behavior for each thing the routes guarantee |

It registers the module in the eleven files that every module shares:

- `modules/mod.rs`;
- `composition/mod.rs`, which mounts its routes, lists its audit actions in the action catalog, and registers its personal data contract;
- the permission matrix in `access/permissions.rs`, with the module's four permissions, such as `order_items.read`, `.create`, `.update` and `.delete`, and their re-export in `access/mod.rs`;
- the access spec's copy of the matrix;
- the accounts spec's two tables of the personal data contracts, with what the module's contract erases and what it exports;
- the list of test modules in `tests/integration/main.rs`, and of the test support's in `support/mod.rs`;
- TEST-08's fixtures in `tests/integration/access.rs`;
- the spec index;
- architecture.md's table of what each module owns and its graph of dependencies, where a module that the document already plans keeps its planned row and edges.

Then it rewrites `.sqlx/` and `openapi.json`, and formats the code. Before it writes anything, it finds the place of each line in each of those files; if a file doesn't hold it, as when a fork has rewritten the file, the task writes nothing at all, and names each line to add by hand, and where.

## Making it the module you need

Work in this order, and run `just ci` once you are done:

1. **The spec first.** Rewrite its Purpose, its table's columns, its routes and its behaviors for what the module is for ([spec README](../spec/README.md#sections)), and the module's row in architecture.md. Each behavior keeps an ID and a test prefix (DOC-08, TEST-01); an ID you delete is never reused, and goes in the spec's line of retired IDs ([spec README](../spec/README.md#ids)).
2. **The table.** Edit the migration for the columns you need, while no release holds it (DATA-09), then run `just migrate` before you write a query that needs them, as [Migrations and the query cache](#migrations-and-the-query-cache) explains.
3. **The code and its tests,** piece by piece, as the next section places them, each piece followed by step 4. Delete what the module doesn't need, such as a route, with its permission, its fixture in TEST-08's test, its behavior and its tests.
4. **The generated files, then the checks.** After a query changes, `just prepare` rewrites `.sqlx/` from the migrated development database, as [Migrations and the query cache](#migrations-and-the-query-cache) explains. After a route changes, `just openapi` rewrites `openapi.json`. Then run `just checks`.

## Migrations and the query cache

Queries are checked while compiling: the tasks build against the query cache in `.sqlx/`, which `just prepare` rewrites from the development database. `migrate` is part of the same binary, so it builds only while the cache holds every query. Run `just migrate` before writing a query, on a fresh clone as after adding a migration, then `just prepare` once it is written, before the other tasks build it. A query written before the database holds its migration keeps the binary from building, `migrate` with it: while the new migration's file is one that git doesn't track yet, `git stash` sets the query aside with every other tracked change, `just migrate` applies the migration, and `git stash pop` brings the changes back. A `just prepare` that fails, as on a query of a table that the database doesn't hold yet, has deleted entries of the cache by then: `git checkout .sqlx` brings them back.

A migration that adds an index to a table that an earlier migration created holds that one statement and nothing else, in a file whose first line is `-- no-transaction`: `CREATE INDEX CONCURRENTLY IF NOT EXISTS <name> ON <table> (...)`, as `migrations/20261116130100_audit_created_at_index.sql` does, with `UNIQUE` before `INDEX` for a unique index (DATA-10). `migrate` runs it outside a transaction, so the build doesn't hold up the table's writes during a deploy; `check-migrations` fails on an index of an earlier table that a migration builds in its transaction, and `migrate` refuses any other statement outside one (PLATFORM-005). An index of a table that the same migration creates stays in that migration.

A migration that adds a foreign key, a check or a `NOT NULL` to a table that an earlier migration created adds it `NOT VALID`, and a migration after it validates it, as `migrations/20261116120000_audit_reading.sql` and the migration after it do (DATA-10): a column becomes `NOT NULL` through `ADD CONSTRAINT <table>_<column>_not_null NOT NULL <column> NOT VALID`. The constraint holds for each row written from the first migration on, and the validation reads the table's rows under a lock that lets writes through; `check-migrations` fails on such a constraint that a migration adds in full, or validates in the migration that adds it, and `check-schema` on one that no migration validated.

A database that a release migrated gets the schema of a new one. One of `just test`'s tests applies the migrations of the newest release, then the migrations as they are, and compares that database's schema with a new database's (DATA-11). A new migration whose timestamp comes before the release's last, as one that a fork ports from the template can, runs after the release's migrations on the first database and among them on the second, and the test fails when the two then differ.

Editing a migration that no release contains (DATA-09) means recreating the development database, since `migrate` refuses a migration whose file changed after it was applied. Recreate it before changing the queries, while the cache in `.sqlx/` still matches them, so the binary compiles without the database: `docker compose exec postgres psql -U postgres -d postgres -c "DROP DATABASE api WITH (FORCE)" -c "CREATE DATABASE api"`, then `just migrate`, which builds against the cache. Once the queries have changed, build the binary first, with `cargo build --package app`, while the old database still holds what they read. Then recreate the database with the same command, run the binary's `bootstrap` and `migrate` with the settings of `.env`, through `just --command target/debug/antworker-api bootstrap` and `just --command target/debug/antworker-api migrate`, and run `just prepare`. The tests' databases need nothing: their template is named after the migrations' checksums. A checksum reads the file's bytes on disk, and `.gitattributes` commits every text file with lines that end in a line feed alone, so save files that way: one whose lines end in a carriage return and a line feed, as Python's text writes do on Windows, commits unchanged, but migrates with a checksum that no checkout reproduces.

## Where each piece goes

| Piece | File | Rules |
|---|---|---|
| A table, its columns, constraints, indexes and comments | a migration named after the module | DATA-01 to DATA-06, DATA-08 to DATA-10, DATA-23 |
| A route: its method, path, documentation and access level | `routes.rs`, and the module's `routes()` | API-01 to API-04, API-14, API-17, ARCH-08 |
| A request's body, and its conversion into domain types | `routes.rs`: a request type, and its `TryFrom` through `FieldErrors` | API-08, API-10 |
| What the API reference says of a route and of its types | the doc comment of the handler, and of each type that derives `ToSchema` and of its fields, which `openapi.json` holds as their descriptions; a note for developers alone, such as why a type derives `Deserialize`, is a `//` comment under it | API-17, DOC-02 |
| A list's paging and search | `routes.rs`: `Page` and `Search`; `repo.rs`: the keyset query | API-05, API-06, API-09, API-20, DATA-17 |
| A row's ID in the path | `routes.rs`: the handler extracts `PathIds`, whose `id` gives `None` for a value that isn't an ID, which answers the module's not-found once the caller is authorized | PLATFORM-072 |
| The response, with its status, `Location` and `ETag` | `routes.rs`: `ResourceResponse`, `ok` or `created`, `tagged` for an editable resource | API-04, API-13 |
| A retry that creates nothing twice | `routes.rs`: `Idempotent<T>`; `service.rs`: `uow.claim`, then `commit_with`, whose result a replay reads back, the next release's replay too, so a field that the result's type gains later is one that a stored result may lack, as `Stored` in `workspaces/model.rs` reads a workspace without its permissions | API-12, DATA-10 |
| An update that a concurrent one can't overwrite | `routes.rs`: `IfMatch`; `repo.rs`: the version in the `WHERE` | API-13, DATA-18 |
| A second member that a `PATCH` edits | `routes.rs`: each member of the patch's request type becomes optional; `service.rs`: a patch that names none changes nothing, and answers the resource as it stands | API-03, API-18, D88, D151 |
| A value's limits and normalization | `model.rs`: a domain type, such as one built on `platform::types::one_line` | API-10, DATA-04, OBS-04 |
| Who may call a route | a permission type and a row of the matrix in `access/permissions.rs`, the route's `workspace::<P>`, and the access spec's matrix | SEC-08, SEC-09 |
| A route that reads across workspaces for a superadmin, or acts on the workspace that its path names | `routes.rs`: `Routes::superadmin`, under `/v1/superadmin/`, whose handler extracts `AuthorizeSuperadmin`, and reads the workspace's ID through `PathIds`; each function that exists only for it, such as one that reads across workspaces, or that first reaches the path's workspace, takes the proof it gives, `Superadmin`, down to the function of its query, and reads only what a superadmin may see, nothing of a workspace's content; what it changes, it records in the caller's own log, under the scope of the proof's `Principal` | API-02, API-14, DATA-16, DATA-22, D62 <!-- part: superadmins --> |
| What the route does | `service.rs`, in the unit of work that `Authorized<P>` opened | ARCH-09, ARCH-11 |
| Its SQL | `repo.rs`, filtered by the workspace from the proof | ARCH-06, ARCH-10, DATA-14 to DATA-16, DATA-20 |
| Its errors, with their codes | `error.rs`, and the codes each route lists in `routes()` | ERR-01 to ERR-06, ERR-13 |
| What it records about a change | `audit.rs`, recorded in the service's unit of work: each action with its name in double quotes right after `Action::new(`, the form in which `check-openapi` reads it, the sentence that tells what it means, the kinds of resource it happens to, the logs that record it, and the type of its details, a struct of IDs and non-personal fields, listed in `ACTIONS`, which `catalog()` in `composition/mod.rs` lists, then `just openapi`. An action that a workspace's log records declares, with `in_workspace_log::<P>()`, the permission of the event that its change publishes, which reading it in the log then takes (the audit spec's [Reading a workspace's log](../spec/audit.md#reading-a-workspaces-log) and [The action catalog](../spec/audit.md#the-action-catalog)) | DATA-22, SEC-18, AUDIT-007, AUDIT-012 to AUDIT-014 |
| What it tells clients over the stream | `events.rs`, published in the service's unit of work: each event with its name in double quotes right after `UserEvent::new(` or `WorkspaceEvent::new(`, the form in which `check-openapi` reads it ([realtime spec](../spec/realtime.md#the-event-catalog)), registered with the module's routes through `Routes::events`, then `just openapi` | API-19, SEC-19 |
| Work after the request, and email | `jobs.rs`, `emails.rs` and its templates ([platform spec](../spec/platform.md#sending-email)) | OPS-09 |
| What it measures | `metrics.rs`, then `just metrics` | OBS-11 to OBS-14 |
| What a user's export holds of its rows, what erasing a deleted user does to them, and which of its columns hold personal data | `personal_data.rs`, registered in `contracts()` of `composition/mod.rs`, the column comments of its migration, and the contract's rows in the accounts spec's tables of [what a deletion erases](../spec/accounts.md#deleting-an-account) and of [what an export holds](../spec/accounts.md#what-an-export-holds), which `check-docs` holds to the contracts that are registered. An erasure deletes, waiting for the operation that holds it, a row of the user's that an operation of the module locks before it names the user, as the [accounts spec's lock order](../spec/accounts.md#deleting-an-account) asks | DATA-23, D31 |
| A setting | the composition root's `Config`, and `.env.example`; for one that a deployment chooses, also `deploy/settings.env.example` and the API's environment in `deploy/compose.yaml`, and, in the answers of the test in `crates/xtask/src/server/questions/mod.rs`, an empty answer at the setting's place in that file's order | OPS-01, OPS-02 |
| Its tests | `tests/integration/<module>.rs`, through the test support's builders, and a fixture in TEST-08's test for each workspace route | TEST-01 to TEST-09 |

The module's own files show each piece working: read its `routes.rs` from a route's handler down to its query.

## Adding a route

To a module that `just new-module` wrote, or to any other:

1. Add the behavior to the module's spec, and the route to its Routes table.
2. If the route needs a permission that the matrix doesn't hold, add its type and its row to `access/permissions.rs`, and its row to the access spec's matrix.
3. If it needs a column or a table, add a migration, or edit the module's own while no release holds it, and run `just migrate`.
4. Write the domain types, the query, the service's function and the handler, as the table above places them, and add the handler to the module's `routes()` with the codes it can answer.
5. Add the route's fixture to the end of `FIXTURES` in TEST-08's test, whose `row` names the test support's builder of the row the route addresses, if it addresses one by ID, then the tests that prove its behavior.
6. Run `just prepare`, `just openapi` and `just checks`.

## Adding a sign-in provider

A user signs in through each OAuth provider that `crates/app/src/modules/auth/providers/` holds a file for ([auth spec](../spec/auth.md#signing-in-through-a-provider)). The registry, that directory's `mod.rs`, does what every provider shares, as OAuth 2.0 and OpenID Connect define it: it reads the settings (AUTH-081), exchanges the code for a token (AUTH-077), and reads the user's identity at the provider's userinfo endpoint. So a provider's file says where the provider is, and replaces only what the provider does differently. Which of two kinds it is decides how much that is:

- **A provider that follows OpenID Connect** is a declaration. It serves a discovery document at `<issuer>/.well-known/openid-configuration`, whose `authorization_endpoint`, `token_endpoint` and `userinfo_endpoint` are the three addresses that the file declares, beside the provider's name and the scopes `openid`, `email` and `profile`.
  Copy `providers/google.rs`, without its `proves` and the claims that `proves` reads, unless the provider says which addresses it is the authority for, as Google does. <!-- part: google -->
- **A provider of OAuth 2.0 alone** serves no such document, and gives a user's identity its own way. Its file replaces `identify`, the read of the identity, and `token` or `discard` where its token endpoint's answers or its tokens differ from what RFC 6749 says.
  Copy `providers/github.rs`, which replaces all three, and `also_redirects_to`, since GitHub sends a user back to more addresses than its app lists (AUTH-102). <!-- part: github -->
  The identity that `identify` reads also holds the address of the user's picture, when the provider gives one, which becomes a new account's avatar (AUTH-092): the registry reads the userinfo endpoint's `picture`, and a file that replaces `identify` reads its provider's own. <!-- part: files -->
  The provider's entry names where its users' pictures come from, in `pictures`: the hosts, as `providers/pictures.rs` describes them, when they are the provider's own, or the setting that lists them, `<KEY>_PICTURE_HOSTS`, when they differ between deployments; a copy of a picture fetches from the hosts that the providers name, and from no other (AUTH-097). <!-- part: files -->

A provider whose addresses differ between deployments, such as an organization's own, reads them from settings, which its entry names in `settings`.
The template has one, which a deployment names with no code, and a product that signs in through a second copies `providers/openid_connect.rs` under a key of its own. <!-- part: openid_connect -->

A provider is an optional part, so each line that exists only for it carries its marker, a comment that reads `part: <key>`, in the forms that ARCH-07 gives for each kind of file, and `just remove <key>` deletes them all. <!-- part: removal -->
The provider's key is in `snake_case`, such as `acme`, of at most 64 characters, the first a letter, as a sign-in's `provider` takes it (AUTH-100), and its settings start with the key in capitals, such as `ACME_`. These are its pieces:

| Piece | File | Rules |
|---|---|---|
| What the provider guarantees: its key, its name, its clients' settings, its scopes, its subject, the addresses it proves, and what becomes of its token | [The auth spec](../spec/auth.md#behaviors), written first: a behavior of its own, with the next ID, a line in the list of providers of the section "Signing in through a provider", and a sentence in each of that section's paragraphs "The identity", "Addresses" and "The provider's token" | DOC-08, D139, D140 |
| The provider: its `ENTRY`, with its key and the prefix of each client's settings, and its `Provider`, which declares its name, its addresses and its scopes | `providers/<key>.rs` | ARCH-13, SEC-10 |
| Its place in the registry | `providers/mod.rs`: `mod <key>;`, and `<key>::ENTRY` in `REGISTRY` | D141 |
| Its tests, each named by its behavior: that it signs in through its addresses with its client, which addresses it proves, and how the capabilities list it. The test support's `Upstream` stands in for its endpoints: the configured client sends every request there and keeps the request's path, so a test sets what each path of the provider's addresses answers | `crates/app/tests/integration/auth/<key>.rs`, and `mod <key>;` in that directory's `mod.rs` | TEST-01, SEC-10 |
| Each client's three settings, `<KEY>_CLIENT_ID`, `<KEY>_CLIENT_SECRET` and `<KEY>_REDIRECT_URIS`, each with its type and its default | `.env.example` | OPS-01, OPS-02 |
| The same settings in the reference deployment: the ID and the redirect uniform resource identifiers (URIs) as settings, and the secret as a file | `deploy/settings.env.example`; `deploy/compose.yaml`, in the API's environment, in its `secrets`, and in the file's `secrets`; `deploy/tasks/secrets.sh`, which writes the secret's file empty; and `deploy/server-setup.sh`, which takes the secret's name | OPS-02, SEC-01 |
| The setup's question for the secret, once the client's ID is set | `crates/xtask/src/server/questions/needed.rs`: an entry of `SECRETS`, and the function that the entry's `needed` names, which says whether the ID is set. In the answers of the test in `questions/mod.rs`, an empty answer for each new setting of `deploy/settings.env.example`, at the setting's place in that file's order | OPS-02 |
| What an operator does at the provider | Its item under [Signing in through a provider](../operations/deploy.md#signing-in-through-a-provider) of deploy.md, and its secret's line in that document's list of what the setup asks for | OPS-02 |
| The part: its two files, its words, its run, and its line in the run `minimal`. Its words are what no line outside the part may hold, compared without regard to case. The provider's name alone is too wide, since prose that stays may hold it, so a new provider's are `<key>_client`, `<key>_redirect`, `providers::<key>`, `providers/<key>` and `mod <key>` | `crates/xtask/parts.yaml`, and its run in the `removable` job of `.github/workflows/ci.yml` | ARCH-07 <!-- part: removal --> |

What the registry asks of a provider's file:

- **`declared`** gives the provider's name, as a client shows it, and its addresses: the authorization endpoint, the token endpoint, and the one that gives a user's identity.
- **`also_redirects_to`** says whether the provider also sends a user back to an address that its client doesn't list, for one that it lists, so that startup refuses another client's redirect URI there, and a redirect URI names one provider (AUTH-081). It says no unless the file replaces it, as it does for a provider that takes more than the URIs that its client lists.
- **`identify`** reads the user's identity with the access token: the `sub`, the `email` and the `name` of the userinfo endpoint's answer, as OpenID Connect gives them. A file replaces it for a provider that gives the identity its own way.
- **`proves`** says whether the provider is the authority of the address it gives, so a sign-in proves the address. It says no unless the file replaces it, which it does only for the addresses that the provider's documentation calls it the authority for (D140): an address that a provider verified once may have changed owner since.
- **`subject`** gives the provider's ID for the user, its userinfo endpoint's `sub`. A file replaces it only when the `sub` alone doesn't name one user for good.
- **`basic`** says whether the token endpoint takes the client's ID and secret in HTTP's Basic scheme. It says no, so they go in the request's body, unless the file replaces it, as it does for a provider whose documentation asks for the scheme.
- **`token`** reads the access token from the token endpoint's answer, as RFC 6749 gives it. A file replaces it for a provider whose answers differ.
- **`discard`** deletes the provider's token once the identity is read. It does nothing unless the file replaces it, which it does for a provider whose tokens don't lapse on their own (D139).

The tests of the behaviors that every provider shares run through a stand-in provider, so a provider's own tests prove only its own behavior.
A provider's test of the picture that a registration through it hands on is named by AUTH-092, and a test of where its entry says its users' pictures come from by AUTH-097. <!-- part: files -->
Run `just check-parts` once the markers are in: it takes under a minute, and names a marker or a word that is wrong. <!-- part: removal -->
The tests that a provider's lines can break run in a few minutes, once `just services` has started what they use:

```
cargo test -p app --locked --test integration -- auth::<key> auth::providers platform::config
cargo test --workspace --locked --lib ops_02    # .env.example states each setting's type and default
cargo test -p xtask --locked                    # the setup's questions
```

Then run `just checks`.
Then `just check-removable <key> minimal` proves that the template passes its checks without the provider. <!-- part: removal -->

## Adding an operator's subcommand

Some changes no route may make, whoever calls it, such as removing a lost authenticator app from an account ([auth spec](../spec/auth.md#second-factors)): an operator makes them on the deployment's server, through a subcommand of the API binary, which `just` runs over Secure Shell (SSH) with the image of the color that serves (ARCH-14, OPS-19). `remove-second-factor` is the one to copy. Its name is in kebab-case, and these are its pieces:

| Piece | File | Rules |
|---|---|---|
| What it guarantees: what it takes, what it changes and records, each reason it changes nothing, and what it logs | The module's spec, written first: its behaviors, its entry of the Interface, and a section of the Design. Its line among the binary's subcommands in [architecture §9](../architecture.md#9-runtime-topology) | DOC-08, ARCH-14 |
| What it does: a function that opens its own unit of work through `Database::for_operator`, since it acts for no request and no job, under an `#[expect]` of clippy's ban, records each change with no actor, under `AuditScope::attempt`, and answers what it did, or why it changed nothing, as types of the module. One that takes an account's address finds the account through `accounts::holder_for_update`, which locks the user's row | The module's `service.rs` and `model.rs`, exported by its `mod.rs` | ARCH-11, DATA-20, DATA-22 |
| The address among the lookups by an email address, for a subcommand that takes one | DATA-16's list in [the data rules](../standards/data.md) | DATA-16 |
| The subcommand: its variant of `Operation`, with what the operator gives it, whose doc comments are its help; its arm of `Operation::run`; its refusal, an error whose variants' messages say why it changed nothing, without personal data, with its variant of `Refused`; and what it answers that it did, which the binary logs | `crates/app/src/composition/tasks.rs`, and the refusal's re-export in `composition/mod.rs` and in `lib.rs`, for the end-to-end test | OPS-03, OBS-04 |
| Its service of each color in the reference deployment, `<subcommand>-blue` and `<subcommand>-green`: each merges `*operator`, which passes the settings that an operator's subcommand reads, and sets its `command` and its color's image | `deploy/compose.yaml` | OPS-02 |
| Its function, which calls `operate`; and its usage, its name in `tasks`, and its arm | `deploy/tasks/accounts.sh`, and `deploy/deploy.sh` | CODE-07, CODE-08 |
| Its task: the recipe; the variant of `ServerTask` and its arm; and the function that runs `deploy.sh` on the server | `just/server.just`; `crates/xtask/src/tasks.rs`; `crates/xtask/src/deploy.rs` | OPS-19 |
| Its tests: each behavior, by running the binary through `TestDatabase::run_api`, with the settings and the assertions of the test support's `operator`; its arm of `subcommand_settings`, and its name among the services that OPS-02's test expects; what its task takes; and its run on a deployment, in `Operated` and `operator_subcommands_run`, with the assertions of what it wrote, in `assert_the_subcommands_ended` | `crates/app/tests/integration/<module>.rs`; `crates/app/tests/integration/platform/config.rs`; the tests of `crates/xtask/src/main.rs`; `crates/app/tests/e2e/deploy.rs` | TEST-01, TEST-03, OPS-02 |
| What an operator does, and when | A section of [deploy.md](../operations/deploy.md) | OPS-19 |

The binary gives the subcommand the database and the deployment's `Mail`, and logs what it answers: one line that says what it did, or the one failure that says why it changed nothing, exiting 1 (OPS-03). Neither holds personal data, so the function answers without the address it was given, and its tests assert that no line holds it (OBS-04). An email that tells the account goes through `Mail::send`, in the unit of work, under `JobScope::attempt` ([platform spec](../spec/platform.md#sending-email)). A deploy's budget of connections counts one for an operator's subcommand, which runs one unit of work at a time (OPS-18).

A subcommand of an optional part carries the part's marker on each of these pieces (ARCH-07). Its arm of `Operation::run` has an expression for its body, never a block, since removing another part can make it the match's last. <!-- part: removal -->

The tests that a subcommand's lines can break run in a few minutes, once `just services` has started what they use:

```
cargo test -p app --locked --test integration -- <module> platform::config platform::startup
cargo test -p xtask --locked                    # what the task takes
just check-shell                                # the deployment's scripts and the recipes
```

Then run `just checks`. `just e2e ops_19` runs it on a deployment, against the image that `just image` builds.

## What the checks keep about the product

A few checks hold facts about the product in lists of their own, which a module edits when it adds what they list. `check-tenancy`, `check-layout` and `check-deps` fail until their lists hold what the module added; `check-layers` can't tell a library that reaches another system from any other, so it checks only those its lists name:

| A module that adds | Edits | Rule |
|---|---|---|
| A table keyed by a user rather than a workspace, such as a user's own preferences, served under `/v1/users/me/` (API-02) | `ACCOUNT_DATA` in `crates/xtask/src/tenancy/mod.rs` | DATA-16 |
| A table whose rows are addressed to a user by an email address, which the user reads before belonging to the workspace | `ADDRESSED` in `crates/xtask/src/tenancy/mod.rs` | DATA-16 |
| A scheduled job that selects rows across workspaces, such as an expiry sweep | `CROSS_TENANT` in `crates/xtask/src/tenancy/mod.rs`, with the file and the name of each function that holds such a query | DATA-16 |
| A table of the module's bookkeeping of what it keeps outside the database, which holds no tenant's data | `BOOKKEEPING` in `crates/xtask/src/tenancy/mod.rs` | DATA-16 |
| A read of workspace data by a row's ID alone, of rows that whoever holds the ID may read, which DATA-16 then lists among its exemptions | `PUBLIC_READS` in `crates/xtask/src/tenancy/mod.rs`, with the file and the name of each function that holds such a query | DATA-16 |
| A library that reaches another system | `INFRASTRUCTURE` in `crates/xtask/src/layers/mod.rs`, and `HTTP_CLIENTS` if it makes HTTP requests | ERR-04, SEC-10 |
| A file outside the set of names a module's files take | a change to ARCH-03, through [Changing a rule](../standards/README.md#changing-a-rule), then `MODULE_FILES`, or `MODULE_EXTRAS` for one module's own entry, in `crates/xtask/src/layout.rs` | ARCH-03 |
| A dependency on another module | the module's edge in the graph in [architecture §5](../architecture.md#dependencies) | ARCH-05 |
