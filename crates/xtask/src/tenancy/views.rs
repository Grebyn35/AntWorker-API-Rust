//! The views that the migrations create, read from their statements' text: what each reads, and
//! the columns its select list shows, which `check-tenancy` checks a view's queries by.

use crate::sql::referenced;

/// A column that a view's select list shows.
#[derive(Debug, PartialEq, Eq)]
pub struct Shown {
    /// The name it shows the column under.
    pub name: String,
    /// The column it shows, for an item that is a column: its name, past a qualifier.
    pub source: Option<String>,
}

/// A view, as its statement creates it.
#[derive(Debug, PartialEq, Eq)]
pub struct View {
    /// The tables and views that its statement names, as `sql::referenced` reads them.
    pub reads: Vec<String>,
    /// What its select list shows, or, where the check can't read it, why.
    pub shown: Result<Vec<Shown>, String>,
}

/// The view `name` whose `CREATE` stands on `line` of `text`, its statement read from the first
/// `VIEW <name>` from that line on, as the doc of `tenancy` says.
pub fn view(text: &str, line: usize, name: &str) -> View {
    let code: String = text
        .lines()
        .skip(line.saturating_sub(1))
        .map(|line| line.split("--").next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    let tokens = tokens(&code);
    let start = tokens
        .windows(2)
        .position(|pair| pair[0] == "view" && pair[1] == name)
        .map_or(tokens.len(), |at| at + 2);
    let mut depth = 0;
    let end = tokens[start..]
        .iter()
        .position(|token| {
            depth += i32::from(token == "(") - i32::from(token == ")");
            depth == 0 && token == ";"
        })
        .map_or(tokens.len(), |end| start + end);
    let statement = &tokens[start..end];
    // The text that `referenced` reads, with each string blanked.
    let sql = statement
        .iter()
        .map(|token| if token.starts_with('\'') { "''" } else { token })
        .collect::<Vec<_>>()
        .join(" ");
    View {
        reads: referenced(&sql),
        shown: shown(statement),
    }
}

/// What the select list of `statement`, the tokens after a view's name, shows, as the doc of
/// `tenancy` says, or why the check can't read it.
fn shown(statement: &[String]) -> Result<Vec<Shown>, String> {
    let at = |index: usize| statement.get(index).map_or("", String::as_str);
    if at(0) != "as" || at(1) != "select" {
        return Err("its name is followed by no `AS SELECT`".to_owned());
    }
    let mut depth = 0;
    for token in statement {
        depth += i32::from(token == "(") - i32::from(token == ")");
        if depth == 0 && ["union", "intersect", "except"].contains(&token.as_str()) {
            return Err(format!(
                "it combines its select list with another's, by `{}`",
                token.to_ascii_uppercase()
            ));
        }
    }
    let mut items = vec![Vec::new()];
    let mut depth = 0;
    for token in &statement[2..] {
        depth += i32::from(token == "(") - i32::from(token == ")");
        match token.as_str() {
            "from" if depth == 0 => break,
            "," if depth == 0 => items.push(Vec::new()),
            _ => {
                if let Some(item) = items.last_mut() {
                    item.push(token);
                }
            }
        }
    }
    items.iter().map(|item| item_shown(item)).collect()
}

/// What `item`, the tokens of one item of a select list, shows, as the doc of `tenancy` says.
fn item_shown(item: &[&String]) -> Result<Shown, String> {
    let words: Vec<&str> = item.iter().map(|token| token.as_str()).collect();
    let name = |word: &str| word.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
    let column = |words: &[&str]| match words {
        [column] if name(column) => Some((*column).to_owned()),
        [qualifier, ".", column] if name(qualifier) && name(column) => Some((*column).to_owned()),
        _ => None,
    };
    if let Some(column) = column(&words) {
        return Ok(Shown {
            name: column.clone(),
            source: Some(column),
        });
    }
    if let [expression @ .., "as", shown] = words.as_slice()
        && !expression.is_empty()
        && name(shown)
    {
        return Ok(Shown {
            name: (*shown).to_owned(),
            source: column(expression),
        });
    }
    Err(format!(
        "its item `{}` is no column, as `column` or `qualifier.column`, and no expression \
         followed by `AS name`",
        words.join(" ")
    ))
}

/// The tokens of `code`: each run of ASCII letters, digits, `_` and `$`, lowercased, each string
/// in single quotes whole, and each other character but white space alone.
pub(super) fn tokens(code: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        let mut token = c.to_ascii_lowercase().to_string();
        if c == '\'' {
            for c in chars.by_ref() {
                token.push(c);
                if c == '\'' {
                    break;
                }
            }
        } else if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
            while let Some(&c) = chars
                .peek()
                .filter(|c| c.is_ascii_alphanumeric() || **c == '_' || **c == '$')
            {
                token.push(c.to_ascii_lowercase());
                chars.next();
            }
        }
        tokens.push(token);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the view `v`, created on the first line of `text`, shows, by its names and sources.
    fn shows(text: &str) -> Result<Vec<(String, Option<String>)>, String> {
        view(text, 1, "v").shown.map(|shown| {
            shown
                .into_iter()
                .map(|shown| (shown.name, shown.source))
                .collect()
        })
    }

    fn column(name: &str, source: Option<&str>) -> (String, Option<String>) {
        (name.to_owned(), source.map(str::to_owned))
    }

    #[test]
    fn data_16_a_view_s_select_list_is_read_by_its_items() {
        let text = "CREATE TABLE x (id uuid); CREATE VIEW v AS SELECT t.id, workspace_id, -- a, b\n\
                    t.user_id AS owner, coalesce(t.at, now()) AS year, name AS user_id, \
                    (SELECT max(id) FROM jobs) AS last\n\
                    FROM things t WHERE name <> ';' AND t.id IN (SELECT id FROM tags);\n\
                    SELECT id FROM notes;";
        assert_eq!(
            shows(text),
            Ok(vec![
                column("id", Some("id")),
                column("workspace_id", Some("workspace_id")),
                column("owner", Some("user_id")),
                column("year", None),
                column("user_id", Some("name")),
                column("last", None),
            ])
        );
        assert_eq!(view(text, 1, "v").reads, ["jobs", "things", "tags"]);
        let text = "CREATE OR REPLACE VIEW v AS\nSELECT id FROM a JOIN b ON a.id = b.id;";
        assert_eq!(view(text, 1, "v").reads, ["a", "b"]);
    }

    #[test]
    fn data_16_a_view_s_select_list_that_isn_t_columns_or_named_expressions_is_refused() {
        let refused = [
            ("SELECT * FROM things", "its item `*`"),
            ("SELECT t.* FROM things t", "its item `t . *`"),
            ("SELECT count(*) FROM things", "its item `count ( * )`"),
            (
                "SELECT workspace_id::text FROM things",
                "its item `workspace_id : : text`",
            ),
            (
                "SELECT 'a' || workspace_id FROM things",
                "its item `'a' | | workspace_id`",
            ),
            (
                "SELECT -workspace_id FROM things",
                "its item `- workspace_id`",
            ),
            (
                "SELECT DISTINCT workspace_id FROM t",
                "its item `distinct workspace_id`",
            ),
            (
                "SELECT workspace_id ws FROM things",
                "its item `workspace_id ws`",
            ),
            (
                "SELECT \"workspace_id\" FROM things",
                "its item `\" workspace_id \"`",
            ),
            (
                "SELECT s.t.workspace_id FROM s.t",
                "its item `s . t . workspace_id`",
            ),
            ("SELECT id, café FROM things", "its item `caf é`"),
            ("SELECT id FROM a UNION SELECT id FROM b", "by `UNION`"),
            ("SELECT id FROM a EXCEPT SELECT id FROM b", "by `EXCEPT`"),
            ("SELECT id, 1 AS union FROM things", "by `UNION`"),
            ("SELECT id UNION SELECT 1", "by `UNION`"),
        ];
        for (select, problem) in refused {
            let shown = shows(&format!("CREATE VIEW v AS {select};"));
            assert!(
                shown.as_ref().is_err_and(|error| error.contains(problem)),
                "{select}: {shown:?}"
            );
        }
        for named in [
            "CREATE VIEW v (a, b) AS SELECT id, workspace_id FROM things;",
            "CREATE VIEW v WITH (security_barrier) AS SELECT id FROM things;",
            "CREATE VIEW v AS (SELECT id FROM things);",
            "CREATE VIEW public.v AS SELECT id FROM things;",
        ] {
            assert_eq!(
                shows(named),
                Err("its name is followed by no `AS SELECT`".to_owned()),
                "{named}"
            );
        }
        let combined = "CREATE VIEW v AS SELECT id FROM a WHERE id IN (SELECT id FROM b UNION \
                        SELECT id FROM c);";
        assert_eq!(shows(combined), Ok(vec![column("id", Some("id"))]));
    }
}
