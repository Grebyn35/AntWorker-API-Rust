# Adding a module

How to add a module that holds a product's data, and how to add a route to a module, new or old. It names where each piece goes and which rules it follows; the rules themselves are in [the standards](../standards/README.md), and how the parts fit together in [architecture.md](../architecture.md).

## Module or route

A module owns tables and the one area of the product they serve ([architecture §5](../architecture.md#what-each-module-owns)). A route over data that a module already owns goes in that module, since no other module may query its tables (ARCH-06): skip to [Adding a route](#adding-a-route). Data a module adds for its product lives in a workspace (DATA-16), which is what `just new-module` writes.

## Writing the module

```
just new-module order_items              # the resource is order_item, the regular singular
just new-module people person            # a resource that the regular plural doesn't give
just new-module categories category      # a name whose ending two kinds of plural share, as movies' does
```

The module's name is plural and the resource's singular, both in `snake_case`. The task refuses a module that the repository has, or had, and its table's or permissions' name if they exist, and a resource whose type the module's code names already, such as `page`, whose `Page` is `platform::lists::Page`, or whose schema name a type of the code takes, such as `role`'s `Role`; it says why, and suggests naming the resource. `just new-module` writes a module that already follows every rule, with one resource: a row of a workspace with a `name`, which the workspace's members create, list, read, rename and delete. It needs Postgres, which it starts, and leaves the development database as it was: `just dev` and `just migrate` apply the new migration to it. A step that fails undoes what the task wrote.

It writes these files, each holding what [architecture §5](../architecture.md#inside-a-module) says its name holds, where `<module>` is the module's name, such as `order_items`:

| File | Holds |
|---|---|
| `migrations/<timestamp>_<module>_<module>.sql` | The module's table, its index for the list, and its column comments |
| `crates/app/src/modules/<module>/` | `mod.rs`, `routes.rs`, `service.rs`, `repo.rs`, `model.rs`, `error.rs`, `events.rs`, `audit.rs`, and `personal_data.rs`, whose contract covers the `name` its migration comments as personal data, exports the rows of each workspace whose only member is the user (D40), and leaves a deleted member's rows to their workspace (DATA-23, D125) |
| `crates/app/tests/integration/<module>.rs` | A test for each behavior |
| `crates/app/tests/integration/support/<module>.rs` | The builder that arranges the module's rows for tests (TEST-09) |
| `docs/spec/<module>.md` | The spec: its routes, and a behavior for each thing the routes guarantee |

It registers the module in the ten files that every module shares: `modules/mod.rs`; `composition/mod.rs`, which mounts its routes and registers its personal data contract; the permission matrix in `access/permissions.rs`, with the module's four permissions, such as `order_items.read`, `.create`, `.update` and `.delete`, and their re-export in `access/mod.rs`; the access spec's copy of the matrix; the list of test modules in `tests/integration/main.rs`, and of the test support's in `support/mod.rs`; TEST-08's fixtures in `tests/integration/access.rs`; the spec index; and architecture.md's table of what each module owns and its graph of dependencies, where a module that the document already plans keeps its planned row and edges. Then it rewrites `.sqlx/` and `openapi.json`, and formats the code. Before it writes anything, it finds the place of each line in each of those files; if a file doesn't hold it, as when a fork has rewritten the file, the task writes nothing at all, and names each line to add by hand, and where.

## Making it the module you need

Work in this order, running `just checks` as you go, and `just ci` once you are done:

1. **The spec first.** Rewrite its Purpose, its table's columns, its routes and its behaviors for what the module is for ([spec README](../spec/README.md#sections)), and the module's row in architecture.md. Each behavior keeps an ID and a test prefix (DOC-08, TEST-01); an ID you delete is never reused, and goes in the spec's line of retired IDs ([spec README](../spec/README.md#ids)).
2. **The table.** Edit the migration for the columns you need, while no release holds it (DATA-09), then run `just migrate` before you write a query that needs them, as [Migrations and the query cache](#migrations-and-the-query-cache) explains.
3. **The code and its tests,** piece by piece, as the next section places them. Delete what the module doesn't need, such as a route, with its permission, its fixture in TEST-08's test, its behavior and its tests.
4. **The generated files.** `just prepare` rewrites `.sqlx/` once the queries compile against the migrated database, and `just openapi` rewrites `openapi.json` once the routes change.

## Migrations and the query cache

Queries are checked while compiling: the tasks build against the query cache in `.sqlx/`, which `just prepare` rewrites from the development database. `migrate` is part of the same binary, so it builds only while the cache holds every query. Run `just migrate` before writing a query, on a fresh clone as after adding a migration, then `just prepare` once it is written, before the other tasks build it.

Editing a migration that no release contains (DATA-09) means recreating the development database, which `migrate` refuses once its applied file differs. Recreate it before changing the queries, while the cache in `.sqlx/` still matches them, so the binary compiles without the database: `docker compose exec postgres psql -U postgres -d postgres -c "DROP DATABASE api WITH (FORCE)" -c "CREATE DATABASE api"`, then `SQLX_OFFLINE=true just migrate`. Once the queries have changed, build the binary first, with `cargo build --package app`, while the old database still holds what they read, then recreate the database and run that binary's `bootstrap` and `migrate` with the settings of `.env`, and then `just prepare`. The tests' databases need nothing: their template is named after the migrations' checksums. A checksum reads the file's bytes on disk, and `.gitattributes` commits every text file with lines that end in a line feed alone, so save files that way: one whose lines end in a carriage return and a line feed, as Python's text writes do on Windows, commits unchanged, but migrates with a checksum that no checkout reproduces.

## Where each piece goes

| Piece | File | Rules |
|---|---|---|
| A table, its columns, constraints, indexes and comments | a migration named after the module | DATA-01 to DATA-06, DATA-08 to DATA-10, DATA-23 |
| A route: its method, path, documentation and access level | `routes.rs`, and the module's `routes()` | API-01 to API-04, API-14, API-17, ARCH-08 |
| A request's body, and its conversion into domain types | `routes.rs`: a request type, and its `TryFrom` through `FieldErrors` | API-08, API-10 |
| A list's paging and search | `routes.rs`: `Page` and `Search`; `repo.rs`: the keyset query | API-05, API-06, API-09, API-20, DATA-17 |
| A row's ID in the path | `routes.rs`: `path_id`, whose `None` answers the module's not-found once the caller is authorized | PLATFORM-072 |
| The response, with its status, `Location` and `ETag` | `routes.rs`: `ResourceResponse`, `ok` or `created`, `tagged` for an editable resource | API-04, API-13 |
| A retry that creates nothing twice | `routes.rs`: `Idempotent<T>`; `service.rs`: `uow.claim`, then `commit_with` | API-12 |
| An update that a concurrent one can't overwrite | `routes.rs`: `IfMatch`; `repo.rs`: the version in the `WHERE` | API-13, DATA-18 |
| A value's limits and normalization | `model.rs`: a domain type, such as one built on `platform::types::one_line` | API-10, DATA-04, OBS-04 |
| Who may call a route | a permission type and a row of the matrix in `access/permissions.rs`, the route's `workspace::<P>`, and the access spec's matrix | SEC-08, SEC-09 |
| What the route does | `service.rs`, in the unit of work that `Authorized<P>` opened | ARCH-09, ARCH-11 |
| Its SQL | `repo.rs`, filtered by the workspace from the proof | ARCH-06, ARCH-10, DATA-14 to DATA-16, DATA-20 |
| Its errors, with their codes | `error.rs`, and the codes each route lists in `routes()` | ERR-01 to ERR-06, ERR-13 |
| What it records about a change | `audit.rs`, recorded in the service's unit of work | DATA-22, SEC-18 |
| What it tells clients over the stream | `events.rs`, published in the service's unit of work | API-19, SEC-19 |
| Work after the request, and email | `jobs.rs`, `emails.rs` and its templates ([platform spec](../spec/platform.md#sending-email)) | OPS-09 |
| What it measures | `metrics.rs`, then `just metrics` | OBS-11 to OBS-14 |
| What a user's export holds of its rows, what erasing a deleted user does to them, and which of its columns hold personal data | `personal_data.rs`, registered in `contracts()` of `composition/mod.rs`, and the column comments of its migration | DATA-23, D31 |
| A setting | the composition root's `Config`, and `.env.example` | OPS-01, OPS-02 |
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

## What the checks keep about the product

A few checks hold facts about the product in lists of their own, which a module edits when it adds what they list. `check-tenancy`, `check-layout` and `check-deps` fail until their lists hold what the module added; `check-layers` can't tell a library that reaches another system from any other, so it checks only those its lists name:

| A module that adds | Edits | Rule |
|---|---|---|
| A table keyed by a user rather than a workspace, such as a user's own preferences, served under `/v1/users/me/` (API-02) | `ACCOUNT_DATA` in `crates/xtask/src/tenancy.rs` | DATA-16 |
| A table whose rows are addressed to a user by an email address, which the user reads before belonging to the workspace | `ADDRESSED` in `crates/xtask/src/tenancy.rs` | DATA-16 |
| A scheduled job that selects rows across workspaces, such as an expiry sweep | `CROSS_TENANT` in `crates/xtask/src/tenancy.rs` | DATA-16 |
| A library that reaches another system | `INFRASTRUCTURE` in `crates/xtask/src/layers.rs`, and `HTTP_CLIENTS` if it makes HTTP requests | ERR-04, SEC-10 |
| A file outside the set of names a module's files take | a change to ARCH-03, through [Changing a rule](../standards/README.md#changing-a-rule), then `MODULE_FILES`, or `MODULE_EXTRAS` for one module's own entry, in `crates/xtask/src/layout.rs` | ARCH-03 |
| A dependency on another module | the module's edge in the graph in [architecture §5](../architecture.md#dependencies) | ARCH-05 |
