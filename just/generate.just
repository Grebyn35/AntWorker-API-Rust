# The tasks that write the files generated from the code and the database: the query cache, the
# OpenAPI document and the metric catalog. The justfile imports this file.

# Rewrites the query cache in .sqlx/ from the migrated database. Run it after changing a query
# (DATA-20). It needs the sqlx-cli of `sqlx_cli_version`: `cargo install sqlx-cli --version
# <version> --no-default-features --features rustls,postgres --locked`.
[doc("Rewrites the query cache in .sqlx/, after a query changes")]
[group('generate')]
prepare: services
    cargo sqlx prepare --workspace -- --all-targets --locked

# Writes openapi.json, the OpenAPI document of every route (API-17). Run it after changing a
# route; a test fails while the committed document differs from the one the code writes.
[doc("Rewrites openapi.json, after a route changes")]
[group('generate')]
openapi:
    {{ api }} openapi openapi.json

# Writes the metric catalog, docs/operations/metrics.md, from the metric declarations (OBS-12).
# Run it after declaring or changing a metric; check-metrics fails while the committed catalog
# differs.
[doc("Rewrites the metric catalog, after a metric changes")]
[group('generate')]
metrics:
    {{ xtask }} metric-catalog
