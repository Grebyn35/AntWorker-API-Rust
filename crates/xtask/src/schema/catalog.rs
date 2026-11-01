//! The catalog of a database that the production `bootstrap` and `migrate` built: a database of
//! its own on the Postgres that `just services` runs, which `bootstrap` gives the roles and their
//! grants, and `migrate` the migrations, run from the API binary as an operator runs them. psql
//! reads the catalog in Postgres's container, and the database is dropped afterwards.

use std::{
    ffi::OsString,
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;

/// The settings the API binary's `bootstrap` and `migrate` read for the database, which `just`
/// loads from `.env`.
const URLS: [&str; 3] = ["DATABASE_ADMIN_URL", "DATABASE_OWNER_URL", "DATABASE_URL"];

/// The catalog, as one JSON value: the tables with their columns, their triggers and the
/// privileges `app` holds on them, the constraints, the indexes, the functions, and the roles'
/// privileges and settings that DATA-12 and DATA-19 give.
const QUERY: &str = r"
WITH app AS (SELECT oid FROM pg_roles WHERE rolname = 'app')
SELECT json_build_object(
  'tables', (SELECT coalesce(json_agg(json_build_object(
      'name', c.relname,
      'owner', pg_get_userbyid(c.relowner),
      'app', (SELECT coalesce(json_agg(p.privilege_type ORDER BY p.privilege_type), '[]')
              FROM aclexplode(coalesce(c.relacl, acldefault('r', c.relowner))) p, app
              WHERE p.grantee = app.oid),
      'columns', (SELECT coalesce(json_agg(json_build_object(
              'name', a.attname, 'type', format_type(a.atttypid, a.atttypmod),
              'not_null', a.attnotnull, 'default', pg_get_expr(d.adbin, d.adrelid))
              ORDER BY a.attnum), '[]')
          FROM pg_attribute a
          LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
          WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped),
      'triggers', (SELECT coalesce(json_agg(json_build_object(
              'function', p.proname,
              'before_update_row', (t.tgtype & 19) = 19)), '[]')
          FROM pg_trigger t JOIN pg_proc p ON p.oid = t.tgfoid
          WHERE t.tgrelid = c.oid AND NOT t.tgisinternal)
    ) ORDER BY c.relname), '[]')
    FROM pg_class c
    WHERE c.relnamespace = 'public'::regnamespace AND c.relkind IN ('r', 'p', 'v', 'm')),
  'constraints', (SELECT coalesce(json_agg(json_build_object(
      'name', con.conname, 'table', rel.relname, 'type', con.contype,
      'on_delete', con.confdeltype,
      'columns', (SELECT coalesce(json_agg(a.attname ORDER BY k.ord), '[]')
          FROM unnest(con.conkey) WITH ORDINALITY k(attnum, ord)
          JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum)
    ) ORDER BY rel.relname, con.conname), '[]')
    FROM pg_constraint con JOIN pg_class rel ON rel.oid = con.conrelid
    WHERE con.connamespace = 'public'::regnamespace),
  'indexes', (SELECT coalesce(json_agg(json_build_object(
      'name', ic.relname, 'table', t.relname, 'unique', i.indisunique,
      'partial', i.indpred IS NOT NULL,
      'constraint', EXISTS (SELECT 1 FROM pg_constraint con WHERE con.conindid = i.indexrelid),
      'columns', (SELECT json_agg(a.attname ORDER BY k.ord)
          FROM unnest(i.indkey::int2[]) WITH ORDINALITY k(attnum, ord)
          LEFT JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
          WHERE k.ord <= i.indnkeyatts)
    ) ORDER BY t.relname, ic.relname), '[]')
    FROM pg_index i
    JOIN pg_class ic ON ic.oid = i.indexrelid
    JOIN pg_class t ON t.oid = i.indrelid
    WHERE t.relnamespace = 'public'::regnamespace),
  'functions', (SELECT coalesce(json_agg(json_build_object(
      'name', p.proname, 'owner', pg_get_userbyid(p.proowner),
      'public_execute', EXISTS (SELECT 1 FROM aclexplode(coalesce(p.proacl, acldefault('f', p.proowner))) a
          WHERE a.grantee = 0 AND a.privilege_type = 'EXECUTE'))), '[]')
    FROM pg_proc p WHERE p.pronamespace = 'public'::regnamespace),
  'types', (SELECT coalesce(json_agg(json_build_object(
      'name', t.typname, 'owner', pg_get_userbyid(t.typowner))), '[]')
    FROM pg_type t WHERE t.typnamespace = 'public'::regnamespace AND t.typtype IN ('e', 'd')),
  'default_function_execute', EXISTS (SELECT 1 FROM aclexplode(coalesce(
      (SELECT defaclacl FROM pg_default_acl
       WHERE defaclrole = 'owner'::regrole AND defaclnamespace = 0 AND defaclobjtype = 'f'),
      acldefault('f', 'owner'::regrole))) a
    WHERE a.grantee = 0 AND a.privilege_type = 'EXECUTE'),
  'app_settings', (SELECT coalesce(json_agg(s), '[]')
    FROM pg_db_role_setting r, unnest(r.setconfig) s, app
    WHERE r.setrole = app.oid AND r.setdatabase = 0),
  'app_creates', has_schema_privilege('app', 'public', 'CREATE'),
  'owner_creates', has_schema_privilege('owner', 'public', 'CREATE'),
  'schema_owner', (SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname = 'public'),
  'database_owner', (SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname = current_database()),
  'app_owns', (SELECT coalesce(json_agg(c.relname), '[]') FROM pg_class c, app WHERE c.relowner = app.oid),
  'app_elsewhere', (SELECT coalesce(json_agg(n.nspname || '.' || c.relname), '[]')
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname NOT IN ('public', 'pg_catalog', 'information_schema')
      AND n.nspname NOT LIKE 'pg\_toast%' AND n.nspname NOT LIKE 'pg\_temp%'
      AND c.relkind IN ('r', 'p', 'v', 'm')
      AND has_table_privilege('app', c.oid, 'SELECT, INSERT, UPDATE, DELETE'))
)";

/// The database's settings, from the environment.
struct Settings {
    /// The three URLs, by setting.
    urls: Vec<(&'static str, String)>,
    /// The database the URLs name, from which the check's database takes its name.
    database: String,
    /// The administrator's user, as which psql connects in the container.
    admin: String,
}

/// Builds a database with the production `bootstrap` and `migrate`, reads its catalog, and drops
/// it.
pub fn read() -> Result<Value, String> {
    let settings = settings()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let name = format!("{}_schema_{}", settings.database, std::process::id());
    psql(
        &root,
        &settings,
        &settings.database,
        &format!("CREATE DATABASE \"{name}\""),
    )?;
    let catalog = build_and_read(&root, &settings, &name);
    let dropped = psql(
        &root,
        &settings,
        &settings.database,
        &format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"),
    );
    let catalog = catalog?;
    dropped?;
    serde_json::from_str(catalog.trim()).map_err(|error| format!("the catalog isn't JSON: {error}"))
}

/// Runs `bootstrap` and `migrate` on the database `name`, then reads its catalog.
fn build_and_read(root: &Path, settings: &Settings, name: &str) -> Result<String, String> {
    for subcommand in ["bootstrap", "migrate"] {
        let mut command =
            Command::new(std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")));
        command
            .args([
                "run",
                "--locked",
                "--quiet",
                "--package",
                "app",
                "--bin",
                "antworker-api",
                "--",
            ])
            .arg(subcommand)
            .current_dir(root);
        for (setting, url) in &settings.urls {
            command.env(setting, with_database(url, name));
        }
        let output = command
            .output()
            .map_err(|error| format!("can't run `{subcommand}`: {error}"))?;
        if !output.status.success() {
            return Err(format!("`{subcommand}` failed: {}", printed(&output)));
        }
    }
    psql(root, settings, name, QUERY)
}

/// Runs `sql` with psql in the container of Compose's `postgres`, on `database`, as the
/// administrator, and returns what it printed.
fn psql(root: &Path, settings: &Settings, database: &str, sql: &str) -> Result<String, String> {
    let output = Command::new("docker")
        .args(["compose", "exec", "--no-TTY", "postgres", "psql"])
        .args(["--no-psqlrc", "--no-align", "--tuples-only", "--quiet"])
        .args([
            "--set",
            "ON_ERROR_STOP=1",
            "--username",
            &settings.admin,
            "--dbname",
            database,
        ])
        .args(["--command", sql])
        .current_dir(root)
        .output()
        .map_err(|error| format!("can't run psql through Docker Compose: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "psql failed; `just services` starts Postgres: {}",
            printed(&output)
        ))
    }
}

/// What a process printed, for its failure.
fn printed(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr).trim(),
        String::from_utf8_lossy(&output.stdout).trim()
    )
}

/// The three URLs from the environment, the database they name and the administrator.
fn settings() -> Result<Settings, String> {
    let mut urls = Vec::new();
    for setting in URLS {
        let url = std::env::var(setting).map_err(|_| {
            format!(
                "{setting} isn't set: run the check through `just check-schema`, which reads .env"
            )
        })?;
        urls.push((setting, url));
    }
    let admin = urls
        .first()
        .map(|(_, url)| url.as_str())
        .unwrap_or_default();
    let database = admin
        .split('?')
        .next()
        .and_then(|url| url.rsplit_once('/'))
        .map(|(_, database)| database.to_owned())
        .filter(|database| !database.is_empty())
        .ok_or("DATABASE_ADMIN_URL names no database")?;
    let user = admin
        .split_once("://")
        .and_then(|(_, rest)| rest.split(['@', ':']).next())
        .map(str::to_owned)
        .filter(|user| !user.is_empty())
        .ok_or("DATABASE_ADMIN_URL names no user")?;
    Ok(Settings {
        urls,
        database,
        admin: user,
    })
}

/// `url` with its database replaced by `database`.
fn with_database(url: &str, database: &str) -> String {
    let (base, query) = url
        .split_once('?')
        .map_or((url, None), |(b, q)| (b, Some(q)));
    let base = base
        .rsplit_once('/')
        .map_or(base, |(authority, _)| authority);
    match query {
        Some(query) => format!("{base}/{database}?{query}"),
        None => format!("{base}/{database}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_19_the_check_s_database_replaces_the_one_each_url_names() {
        assert_eq!(
            with_database("postgres://app:pw@127.0.0.1:5432/api", "api_schema_7"),
            "postgres://app:pw@127.0.0.1:5432/api_schema_7"
        );
        assert_eq!(
            with_database("postgres://owner@db/api?sslmode=require", "api_schema_7"),
            "postgres://owner@db/api_schema_7?sslmode=require"
        );
    }
}
