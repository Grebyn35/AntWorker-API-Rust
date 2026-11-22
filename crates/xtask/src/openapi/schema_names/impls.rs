//! The names that a hand-written impl of `ToSchema` gives, as the comment of
//! [`names`](super::super::names) says the check reads them.

use proc_macro2::{Delimiter, Spacing, TokenTree};

use crate::Finding;

use super::{
    APP, MacroRules, Reading, Schema, Source,
    elsewhere::{bound, constants, invoked, takes_first},
    is_ident, line_of, refusal,
};

/// What a hand-written `ToSchema`'s `name()` returns.
enum Named {
    /// No `name()`: utoipa's, which gives the type's name without its generic arguments.
    Default,
    /// `Cow::Borrowed("…")`.
    Literal(String),
    /// `Cow::Borrowed(stringify!($fragment))`.
    Fragment(String),
    /// `Cow::Borrowed(P::CONSTANT)`, of a type parameter `P`.
    Constant(String, String),
}

/// The names that each `ToSchema for` among `tokens`, a part of `source`'s, gives, at any depth,
/// inside the `macro_rules!` that `rules` is, if one holds them.
pub(super) fn implemented(
    source: &Source,
    tokens: &[TokenTree],
    rules: Option<&MacroRules>,
    sources: &[Source],
    reading: &mut Reading,
) {
    let mut at = 0;
    while let Some(token) = tokens.get(at) {
        match token {
            TokenTree::Ident(keyword) if keyword == "macro_rules" => {
                if let (
                    Some(TokenTree::Punct(bang)),
                    Some(TokenTree::Ident(name)),
                    Some(TokenTree::Group(body)),
                ) = (tokens.get(at + 1), tokens.get(at + 2), tokens.get(at + 3))
                    && bang.as_char() == '!'
                {
                    let body: Vec<TokenTree> = body.stream().into_iter().collect();
                    let definition = MacroRules {
                        name: name.to_string(),
                        matchers: matchers(&body),
                    };
                    implemented(source, &body, Some(&definition), sources, reading);
                    at += 4;
                    continue;
                }
            }
            TokenTree::Ident(ident)
                if ident == "ToSchema" && is_ident(tokens.get(at + 1), "for") =>
            {
                if line_of(token) <= source.before {
                    implementation(source, tokens, at, rules, sources, reading);
                }
            }
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                implemented(source, &inner, rules, sources, reading);
            }
            _ => {}
        }
        at += 1;
    }
}

/// An impl of `ToSchema`, read.
struct Impl<'a> {
    /// Its file.
    path: &'a str,
    /// The line of its `ToSchema`.
    line: usize,
    /// Its type's name, or `$` and the fragment's that names its type.
    type_name: String,
    /// The fragment that names its type, in a `macro_rules!`.
    fragment: Option<String>,
    /// Its tokens from `impl` to its body.
    head: &'a [TokenTree],
}

impl Impl<'_> {
    /// The schema `name`, which the check reads at `line` of the file at `path` for this impl.
    fn schema(&self, name: String, path: &str, line: usize) -> Schema {
        Schema {
            type_name: self.type_name.clone(),
            name,
            path: path.to_owned(),
            line,
            in_app: self.path.starts_with(APP),
            generic: false,
        }
    }

    /// A finding that refuses this impl, for `why`.
    fn refused(&self, why: &str) -> Finding {
        refusal(self.path, self.line, &self.type_name, why)
    }
}

/// The names that the impl whose `ToSchema` is `tokens[at]` gives.
fn implementation(
    source: &Source,
    tokens: &[TokenTree],
    at: usize,
    rules: Option<&MacroRules>,
    sources: &[Source],
    reading: &mut Reading,
) {
    let line = line_of(&tokens[at]);
    let (type_name, fragment) = match (tokens.get(at + 2), tokens.get(at + 3)) {
        (Some(TokenTree::Ident(name)), _) => (name.to_string(), None),
        (Some(TokenTree::Punct(dollar)), Some(TokenTree::Ident(name)))
            if dollar.as_char() == '$' =>
        {
            (format!("${name}"), Some(name.to_string()))
        }
        _ => {
            reading
                .refused
                .push(refusal(source.path, line, "?", "its type isn't a name"));
            return;
        }
    };
    let start = tokens[..at]
        .iter()
        .rposition(|token| is_ident(Some(token), "impl"))
        .unwrap_or(at);
    let end = tokens[at..]
        .iter()
        .position(|token| matches!(token, TokenTree::Group(g) if g.delimiter() == Delimiter::Brace))
        .map_or(tokens.len(), |offset| at + offset);
    let named = match tokens.get(end) {
        Some(TokenTree::Group(body)) => {
            name_returned(&body.stream().into_iter().collect::<Vec<TokenTree>>())
        }
        _ => Some(Named::Default),
    };
    let found = Impl {
        path: source.path,
        line,
        type_name,
        fragment,
        head: &tokens[start..end],
    };
    give(&found, named, rules, sources, reading);
}

/// The names that `found` gives, whose `name()` returns `named`, inside the `macro_rules!` that
/// `rules` is, if one holds it.
fn give(
    found: &Impl,
    named: Option<Named>,
    rules: Option<&MacroRules>,
    sources: &[Source],
    reading: &mut Reading,
) {
    match (named, found.fragment.clone()) {
        (Some(Named::Default), None) => {
            let schema = found.schema(found.type_name.clone(), found.path, found.line);
            reading.schemas.push(schema);
        }
        (Some(Named::Literal(name)), _) => {
            let schema = found.schema(name, found.path, found.line);
            reading.schemas.push(schema);
        }
        (Some(Named::Default), Some(fragment)) | (Some(Named::Fragment(fragment)), _) => {
            match rules.filter(|rules| takes_first(rules, &fragment)) {
                Some(rules) => {
                    let in_app = found.path.starts_with(APP);
                    invoked(&rules.name, sources, reading, in_app);
                }
                None => reading.refused.push(found.refused(&format!(
                    "it names its schema by `${fragment}`, which isn't the first fragment of kind \
                     `ident`, after those of kinds `meta` and `vis`, of each rule of a \
                     `macro_rules!` that holds it"
                ))),
            }
        }
        (Some(Named::Constant(parameter, constant)), _) => match bound(found.head, &parameter) {
            Some(bound) => {
                for (path, line, value) in constants(sources, &bound, &constant, reading) {
                    reading.schemas.push(found.schema(value, path, line));
                }
            }
            None => reading.refused.push(found.refused(&format!(
                "`{parameter}` isn't a type parameter with a trait's bound"
            ))),
        },
        (None, _) => reading.refused.push(found.refused(
            "its `name()` returns neither `Cow::Borrowed` of a string literal, nor of \
             `stringify!($fragment)`, nor of a type parameter's constant",
        )),
    }
}

/// What the `fn name` of `body`, an impl's, returns, or `None` when it returns another form.
fn name_returned(body: &[TokenTree]) -> Option<Named> {
    let Some(at) = body
        .windows(2)
        .position(|pair| is_ident(pair.first(), "fn") && is_ident(pair.get(1), "name"))
    else {
        return Some(Named::Default);
    };
    let function = body[at..].iter().find_map(|token| match token {
        TokenTree::Group(group) if group.delimiter() == Delimiter::Brace => {
            Some(group.stream().into_iter().collect::<Vec<TokenTree>>())
        }
        _ => None,
    })?;
    let borrowed = function
        .iter()
        .position(|token| is_ident(Some(token), "Borrowed"))?;
    let Some(TokenTree::Group(argument)) = function.get(borrowed + 1) else {
        return None;
    };
    let argument: Vec<TokenTree> = argument.stream().into_iter().collect();
    match argument.as_slice() {
        [TokenTree::Literal(literal)] => syn::parse_str::<syn::LitStr>(&literal.to_string())
            .ok()
            .map(|literal| Named::Literal(literal.value())),
        [
            TokenTree::Ident(macro_name),
            TokenTree::Punct(bang),
            TokenTree::Group(group),
        ] if macro_name == "stringify" && bang.as_char() == '!' => {
            match group
                .stream()
                .into_iter()
                .collect::<Vec<TokenTree>>()
                .as_slice()
            {
                [TokenTree::Punct(dollar), TokenTree::Ident(fragment)]
                    if dollar.as_char() == '$' =>
                {
                    Some(Named::Fragment(fragment.to_string()))
                }
                _ => None,
            }
        }
        [
            TokenTree::Ident(parameter),
            TokenTree::Punct(first),
            TokenTree::Punct(second),
            TokenTree::Ident(constant),
        ] if first.as_char() == ':'
            && first.spacing() == Spacing::Joint
            && second.as_char() == ':' =>
        {
            Some(Named::Constant(parameter.to_string(), constant.to_string()))
        }
        _ => None,
    }
}

/// The matcher of each rule of a `macro_rules!` whose body is `body`.
fn matchers(body: &[TokenTree]) -> Vec<Vec<TokenTree>> {
    body.split(|token| matches!(token, TokenTree::Punct(semicolon) if semicolon.as_char() == ';'))
        .filter_map(|rule| match rule.first() {
            Some(TokenTree::Group(matcher)) => Some(matcher.stream().into_iter().collect()),
            _ => None,
        })
        .collect()
}
