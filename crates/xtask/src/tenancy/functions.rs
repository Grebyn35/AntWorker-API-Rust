//! The queries of a file of Rust read as token streams, each with the function that holds it,
//! which `check-tenancy` exempts by, and the names of the file's functions.

use proc_macro2::{Delimiter, TokenTree};

/// The `sqlx` macros that take SQL text (DATA-14).
const MACROS: [&str; 6] = [
    "query",
    "query_as",
    "query_scalar",
    "query_unchecked",
    "query_as_unchecked",
    "query_scalar_unchecked",
];

/// A query in a `sqlx` macro.
pub struct Query {
    /// Its SQL text: the first string literal among the macro's arguments.
    pub sql: String,
    /// The line of that literal.
    pub line: usize,
    /// The name of the innermost `fn` whose body holds the macro, if one does.
    pub function: Option<String>,
}

/// What a file's tokens hold: each query of a `sqlx` macro, and the name of each `fn`, at any
/// depth.
#[derive(Default)]
pub struct Held {
    /// The queries, in the order of the file.
    pub queries: Vec<Query>,
    /// The name of each `fn` followed by a name, once for each.
    pub functions: Vec<String>,
    /// The line of each macro named as one of [`MACROS`] in another form than
    /// `sqlx::<macro>!( ... )`, the one whose query [`invoked`] reads.
    pub unread: Vec<usize>,
}

/// The queries and the functions of `tokens`, each query with the function that holds it, as the
/// doc of `tenancy` says.
pub fn held(tokens: &[TokenTree]) -> Held {
    let mut held = Held::default();
    walk(tokens, None, &mut held);
    held
}

/// The SQL text of each `sqlx` macro in `tokens`, at any depth, with its line.
pub fn sql_texts(tokens: &[TokenTree], found: &mut Vec<(String, usize)>) {
    found.extend(
        held(tokens)
            .queries
            .into_iter()
            .map(|query| (query.sql, query.line)),
    );
}

/// Adds the queries and the functions of `tokens` to `held`, those outside any `fn`'s body among
/// them as `function`'s.
fn walk(tokens: &[TokenTree], function: Option<&str>, held: &mut Held) {
    // The `fn` whose body is the next group in braces at this level.
    let mut named: Option<String> = None;
    for (index, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Ident(keyword) if keyword == "fn" => {
                named = match tokens.get(index + 1) {
                    Some(TokenTree::Ident(name)) => Some(name.to_string()),
                    _ => None,
                };
                held.functions.extend(named.clone());
            }
            TokenTree::Punct(punct) if punct.as_char() == ';' => named = None,
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                if group.delimiter() == Delimiter::Brace
                    && let Some(name) = named.take()
                {
                    walk(&inner, Some(&name), held);
                    continue;
                }
                if let Some((sql, line)) = invoked(tokens, index, &inner) {
                    held.queries.push(Query {
                        sql,
                        line,
                        function: function.map(str::to_owned),
                    });
                } else if let [TokenTree::Ident(name), TokenTree::Punct(bang), _] =
                    &tokens[index.saturating_sub(2)..=index]
                    && bang.as_char() == '!'
                    && MACROS.contains(&name.to_string().as_str())
                {
                    held.unread.push(name.span().start().line);
                }
                walk(&inner, function, held);
            }
            _ => {}
        }
    }
}

/// The SQL text and its line, where the group at `index` of `tokens`, whose tokens are `inner`,
/// holds the arguments of a `sqlx` macro: `sqlx::<macro>!( ... )`.
fn invoked(tokens: &[TokenTree], index: usize, inner: &[TokenTree]) -> Option<(String, usize)> {
    let TokenTree::Group(group) = &tokens[index] else {
        return None;
    };
    let invoked = index >= 2
        && group.delimiter() == Delimiter::Parenthesis
        && matches!(&tokens[index - 1], TokenTree::Punct(punct) if punct.as_char() == '!')
        && matches!(&tokens[index - 2], TokenTree::Ident(name) if MACROS.contains(&name.to_string().as_str()));
    let in_sqlx = index >= 5
        && matches!(&tokens[index - 5..index - 2], [TokenTree::Ident(krate), TokenTree::Punct(_), TokenTree::Punct(_)] if *krate == "sqlx");
    if !(invoked && in_sqlx) {
        return None;
    }
    inner.iter().find_map(|token| match token {
        TokenTree::Literal(literal) => syn::parse_str::<syn::LitStr>(&literal.to_string())
            .ok()
            .map(|text| (text.value(), literal.span().start().line)),
        _ => None,
    })
}
