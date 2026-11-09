//! The catalog of a database that the production `bootstrap` and `migrate` built
//! (`migrated`). psql reads it in Postgres's container, and the database is dropped afterwards.

use serde_json::Value;

use crate::migrated;

/// The catalog, as one JSON value: the tables with their columns, their triggers and the
/// privileges `app` holds on them, the sequences with theirs, the constraints, the indexes, the
/// functions, and the roles' privileges and settings that DATA-12 and DATA-19 give.
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
  'sequences', (SELECT coalesce(json_agg(json_build_object(
      'name', c.relname,
      'owner', pg_get_userbyid(c.relowner),
      'app', (SELECT coalesce(json_agg(p.privilege_type ORDER BY p.privilege_type), '[]')
              FROM aclexplode(coalesce(c.relacl, acldefault('s', c.relowner))) p, app
              WHERE p.grantee = app.oid)
    ) ORDER BY c.relname), '[]')
    FROM pg_class c
    WHERE c.relnamespace = 'public'::regnamespace AND c.relkind = 'S'),
  'constraints', (SELECT coalesce(json_agg(json_build_object(
      'name', con.conname, 'table', rel.relname, 'type', con.contype,
      'on_delete', con.confdeltype,
      'references', (SELECT r.relname FROM pg_class r WHERE r.oid = con.confrelid),
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
      'trigger', p.prorettype = 'trigger'::regtype, 'security_definer', p.prosecdef,
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

/// Builds a database with the production `bootstrap` and `migrate`, reads its catalog, and drops
/// it.
pub fn read() -> Result<Value, String> {
    let catalog = migrated::with_database("schema", |database| database.psql(QUERY))?;
    serde_json::from_str(catalog.trim()).map_err(|error| format!("the catalog isn't JSON: {error}"))
}
