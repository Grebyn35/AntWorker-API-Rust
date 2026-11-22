//! The names that an impl of `ToSchema` takes from other code: the invocations of the
//! `macro_rules!` that holds it, and the constants that the impls of its type parameter's trait
//! give, as the comment of [`names`](super::super::names) says the check reads them.

use proc_macro2::{Delimiter, Spacing, TokenTree};

use super::{MacroRules, Reading, Schema, Source, is_ident, line_of, refusal};

/// Whether each of `rules`' matchers takes `$fragment:ident` as its first fragment of kind
/// `ident`, after fragments of kinds `meta` and `vis` alone.
pub(super) fn takes_first(rules: &MacroRules, fragment: &str) -> bool {
    !rules.matchers.is_empty()
        && rules.matchers.iter().all(|matcher| {
            let mut fragments = Vec::new();
            fragments_of(matcher, &mut fragments);
            fragments
                .iter()
                .find(|(_, kind)| kind != "meta" && kind != "vis")
                .is_some_and(|(name, kind)| name == fragment && kind == "ident")
        })
}

/// Each fragment, `$name:kind`, of `matcher`, at any depth, in order.
fn fragments_of(matcher: &[TokenTree], fragments: &mut Vec<(String, String)>) {
    for (at, token) in matcher.iter().enumerate() {
        match (
            token,
            matcher.get(at + 1),
            matcher.get(at + 2),
            matcher.get(at + 3),
        ) {
            (
                TokenTree::Punct(dollar),
                Some(TokenTree::Ident(name)),
                Some(TokenTree::Punct(colon)),
                Some(TokenTree::Ident(kind)),
            ) if dollar.as_char() == '$' && colon.as_char() == ':' => {
                fragments.push((name.to_string(), kind.to_string()));
            }
            (TokenTree::Group(group), ..) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                fragments_of(&inner, fragments);
            }
            _ => {}
        }
    }
}

/// The names that the invocations of the macro `name` give, among `sources`: each the first
/// identifier of its arguments after their attributes and a `pub` with its restriction.
pub(super) fn invoked(name: &str, sources: &[Source], reading: &mut Reading, in_app: bool) {
    for source in sources {
        let mut found = Vec::new();
        invocations(&source.tokens, name, &mut found);
        for (line, arguments) in found {
            if line > source.before {
                continue;
            }
            match first_name(&arguments) {
                Some((type_name, line)) => reading.schemas.push(Schema {
                    name: type_name.clone(),
                    type_name,
                    path: source.path.to_owned(),
                    line,
                    in_app,
                    generic: false,
                }),
                None => reading.refused.push(refusal(
                    source.path,
                    line,
                    &format!("{name}!"),
                    "its arguments start with no name after their attributes and a `pub`",
                )),
            }
        }
    }
}

/// The line and the arguments of each invocation `name!(…)` among `tokens`, at any depth.
fn invocations(tokens: &[TokenTree], name: &str, found: &mut Vec<(usize, Vec<TokenTree>)>) {
    for (at, token) in tokens.iter().enumerate() {
        match (token, tokens.get(at + 1), tokens.get(at + 2)) {
            (
                TokenTree::Ident(ident),
                Some(TokenTree::Punct(bang)),
                Some(TokenTree::Group(group)),
            ) if ident == name && bang.as_char() == '!' => {
                found.push((line_of(token), group.stream().into_iter().collect()));
            }
            (TokenTree::Group(group), ..) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                invocations(&inner, name, found);
            }
            _ => {}
        }
    }
}

/// The first identifier of `arguments` after their attributes and a `pub` with its restriction,
/// with its line.
fn first_name(arguments: &[TokenTree]) -> Option<(String, usize)> {
    let mut at = 0;
    loop {
        match (arguments.get(at), arguments.get(at + 1)) {
            (Some(TokenTree::Punct(hash)), Some(TokenTree::Group(_))) if hash.as_char() == '#' => {
                at += 2;
            }
            (Some(TokenTree::Ident(public)), restriction) if public == "pub" => {
                let restricted = matches!(
                    restriction,
                    Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Parenthesis
                );
                at += if restricted { 2 } else { 1 };
            }
            (Some(token @ TokenTree::Ident(name)), _) => {
                return Some((name.to_string(), line_of(token)));
            }
            _ => return None,
        }
    }
}

/// The trait that bounds `parameter` among `tokens`, an impl's from `impl` to its body: the last
/// segment of the path after `parameter:`.
pub(super) fn bound(tokens: &[TokenTree], parameter: &str) -> Option<String> {
    let at = tokens.windows(2).position(|pair| {
        is_ident(pair.first(), parameter)
            && matches!(pair.get(1), Some(TokenTree::Punct(colon))
                if colon.as_char() == ':' && colon.spacing() == Spacing::Alone)
    })?;
    let mut segment = None;
    let mut next = at + 2;
    while let Some(TokenTree::Ident(ident)) = tokens.get(next) {
        segment = Some(ident.to_string());
        let separated = matches!(
            (tokens.get(next + 1), tokens.get(next + 2)),
            (Some(TokenTree::Punct(a)), Some(TokenTree::Punct(b)))
                if a.as_char() == ':' && a.spacing() == Spacing::Joint && b.as_char() == ':'
        );
        if !separated {
            break;
        }
        next += 3;
    }
    segment
}

/// The file, the line and the value of the constant `constant` that each impl of the trait
/// `bound` among `sources` gives as a string literal; a finding for each impl that doesn't.
pub(super) fn constants<'a>(
    sources: &'a [Source],
    bound: &str,
    constant: &str,
    reading: &mut Reading,
) -> Vec<(&'a str, usize, String)> {
    let mut found = Vec::new();
    for source in sources {
        let mut bodies = Vec::new();
        trait_impls(&source.tokens, bound, &mut bodies);
        for (line, body) in bodies
            .into_iter()
            .filter(|(line, _)| *line <= source.before)
        {
            match given(&body, constant) {
                Some((line, value)) => found.push((source.path, line, value)),
                None => reading.refused.push(refusal(
                    source.path,
                    line,
                    &format!("{bound} for"),
                    &format!("it doesn't give `{constant}` as a string literal"),
                )),
            }
        }
    }
    found
}

/// The line and the value of `const <constant>: … = "…";` among `body`, an impl's.
fn given(body: &[TokenTree], constant: &str) -> Option<(usize, String)> {
    let at = body
        .windows(2)
        .position(|pair| is_ident(pair.first(), "const") && is_ident(pair.get(1), constant))?;
    let rest = &body[at + 2..];
    let equals = rest
        .iter()
        .position(|token| matches!(token, TokenTree::Punct(punct) if punct.as_char() == '='))?;
    match (rest.get(equals + 1), rest.get(equals + 2)) {
        (Some(token @ TokenTree::Literal(literal)), Some(TokenTree::Punct(end)))
            if end.as_char() == ';' =>
        {
            let value = syn::parse_str::<syn::LitStr>(&literal.to_string())
                .ok()?
                .value();
            Some((line_of(token), value))
        }
        _ => None,
    }
}

/// The line and the body of each `<bound> for …` among `tokens`, at any depth.
fn trait_impls(tokens: &[TokenTree], bound: &str, found: &mut Vec<(usize, Vec<TokenTree>)>) {
    for (at, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Ident(ident) if ident == bound && is_ident(tokens.get(at + 1), "for") => {
                if let Some(body) = tokens[at..].iter().find_map(|token| match token {
                    TokenTree::Group(group) if group.delimiter() == Delimiter::Brace => {
                        Some(group.stream().into_iter().collect::<Vec<TokenTree>>())
                    }
                    _ => None,
                }) {
                    found.push((line_of(token), body));
                }
            }
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                trait_impls(&inner, bound, found);
            }
            _ => {}
        }
    }
}
