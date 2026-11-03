//! The tables a query names, which `check-tenancy`, `check-table-ownership` and `check-shell`
//! read from the SQL of the code and the scripts.

/// The tables that `sql`, lowercase and with its strings blanked, reads or writes: the name after
/// `FROM`, `JOIN`, `INTO`, `UPDATE` and a `USING` that names no columns, past `ONLY` and a
/// schema's name, and each further name of a `FROM` or `USING` list, after an alias.
pub fn referenced(sql: &str) -> Vec<String> {
    /// The words that end a table's alias, where an alias would stand.
    const CLAUSES: [&str; 24] = [
        "where",
        "join",
        "inner",
        "left",
        "right",
        "full",
        "cross",
        "natural",
        "on",
        "using",
        "group",
        "order",
        "limit",
        "offset",
        "returning",
        "for",
        "set",
        "values",
        "union",
        "having",
        "window",
        "except",
        "intersect",
        "select",
    ];
    let tokens = sql_tokens(sql);
    let at = |index: usize| tokens.get(index).map_or("", String::as_str);
    let mut tables = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let listed = token == "from" || token == "using";
        if !(listed || ["join", "into", "update"].contains(&token.as_str()))
            || (token == "using" && at(index + 1) == "(")
        {
            continue;
        }
        let mut next = index + 1;
        loop {
            if at(next) == "only" {
                next += 1;
            }
            if at(next + 1) == "." {
                next += 2;
            }
            let name = at(next);
            if !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
                break;
            }
            next += 1;
            if at(next) == "(" && token != "into" && token != "update" {
                // A function, such as `unnest($1)`, whose rows a list may join with tables;
                // after `INTO`, the parentheses list the columns.
                let mut depth = 0;
                while next < tokens.len() {
                    depth += match at(next) {
                        "(" => 1,
                        ")" => -1,
                        _ => 0,
                    };
                    next += 1;
                    if depth == 0 {
                        break;
                    }
                }
            } else {
                tables.push(name.to_owned());
            }
            if at(next) == "as" {
                next += 1;
            }
            let alias = at(next);
            if alias.starts_with(|c: char| c.is_ascii_alphabetic()) && !CLAUSES.contains(&alias) {
                next += 1;
            }
            if !(listed && at(next) == ",") {
                break;
            }
            next += 1;
        }
    }
    tables
}

/// The words of `sql` and its punctuation `,`, `.`, `(` and `)`, each a token of its own,
/// outside quoted strings and comments. A parameter, such as `$1`, is a word.
fn sql_tokens(sql: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for line in sql.lines() {
        let code = line.split("--").next().unwrap_or_default();
        let mut quoted = false;
        let mut word = String::new();
        for c in code.chars().chain(std::iter::once(' ')) {
            if c == '\'' {
                quoted = !quoted;
            }
            if !quoted && (c.is_ascii_alphanumeric() || c == '_' || c == '$') {
                word.push(c.to_ascii_lowercase());
                continue;
            }
            if !word.is_empty() {
                tokens.push(std::mem::take(&mut word));
            }
            if !quoted && ",.()".contains(c) {
                tokens.push(c.to_string());
            }
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_06_a_query_s_tables_are_read_from_lists_schemas_and_using() {
        let sql = "select u.id from public.users u, sessions as s join only workspaces w on true \
                   where exists (select 1 from unnest($1) as n, tags) \
                   and u.id in (select user_id from events using (id))";
        assert_eq!(
            referenced(sql),
            ["users", "sessions", "workspaces", "tags", "events"]
        );
        assert_eq!(
            referenced("delete from jobs using audit_entries a where a.id = jobs.id"),
            ["jobs", "audit_entries"]
        );
        assert_eq!(
            referenced("insert into jobs (kind, payload) values ($1, $2)"),
            ["jobs"]
        );
        assert_eq!(
            referenced("update rate_limit_buckets set count = count + 1, window = now()"),
            ["rate_limit_buckets"]
        );
    }
}
