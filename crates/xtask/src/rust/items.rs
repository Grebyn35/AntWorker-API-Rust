//! The items of Rust source read as token streams: functions' signatures, the fields public
//! types expose, and the attributes of structs and enums.

use proc_macro2::{Delimiter, TokenTree};

use super::split;

/// A function's signature: from `fn` to its body or its `;`.
pub struct Signature {
    /// Whether it is visible outside its module: `pub`, with or without a restriction.
    pub public: bool,
    /// The line of `fn`.
    pub line: usize,
    /// The tokens of its name, generics, parameters, result and `where` clause.
    pub tokens: Vec<TokenTree>,
}

/// The signature of every function among `tokens`, at any depth, those of methods and of
/// functions inside other functions among them.
pub fn signatures(tokens: &[TokenTree]) -> Vec<Signature> {
    let mut found = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Ident(keyword) if keyword == "fn" => {
                let end = tokens[at..]
                    .iter()
                    .position(|token| match token {
                        TokenTree::Group(group) => group.delimiter() == Delimiter::Brace,
                        TokenTree::Punct(semicolon) => semicolon.as_char() == ';',
                        _ => false,
                    })
                    .map_or(tokens.len(), |end| at + end);
                found.push(Signature {
                    public: is_public(&tokens[..at]),
                    line: keyword.span().start().line,
                    tokens: tokens[at + 1..end].to_vec(),
                });
            }
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                found.extend(signatures(&inner));
            }
            _ => {}
        }
    }
    found
}

/// Whether the item whose keyword follows `before` is `pub`: the qualifiers `async`, `const`,
/// `unsafe` and `extern "abi"` may stand between.
pub(super) fn is_public(before: &[TokenTree]) -> bool {
    let mut at = before.len();
    while at > 0 {
        at -= 1;
        match &before[at] {
            TokenTree::Ident(ident) if ident == "pub" => return true,
            TokenTree::Ident(ident)
                if ["async", "const", "unsafe", "extern", "default"]
                    .contains(&ident.to_string().as_str()) => {}
            TokenTree::Literal(_) => {}
            TokenTree::Group(group) if group.delimiter() == Delimiter::Parenthesis => {}
            _ => return false,
        }
    }
    false
}

/// The tokens of the fields a type makes public: every field of a `pub` enum's variants, and
/// the `pub` fields of a `pub` struct, each with the line of its type's keyword.
pub fn public_fields(tokens: &[TokenTree]) -> Vec<(usize, Vec<TokenTree>)> {
    let mut found = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Ident(keyword)
                if (keyword == "enum" || keyword == "struct") && is_public(&tokens[..at]) =>
            {
                let line = keyword.span().start().line;
                let mut body = None;
                for token in &tokens[at..] {
                    match token {
                        TokenTree::Punct(semicolon) if semicolon.as_char() == ';' => break,
                        TokenTree::Group(group) if group.delimiter() != Delimiter::Bracket => {
                            body = Some(group.stream().into_iter().collect::<Vec<TokenTree>>());
                            break;
                        }
                        _ => {}
                    }
                }
                let Some(body) = body else { continue };
                if keyword == "enum" {
                    found.push((line, body));
                } else {
                    for field in split(&body) {
                        if field
                            .iter()
                            .any(|t| matches!(t, TokenTree::Ident(i) if i == "pub"))
                        {
                            found.push((line, field.to_vec()));
                        }
                    }
                }
            }
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                found.extend(public_fields(&inner));
            }
            _ => {}
        }
    }
    found
}

/// Each struct and enum among `tokens`, at any depth, with its name, its line, and the
/// attributes written before it.
pub fn types_with_attributes(tokens: &[TokenTree]) -> Vec<(String, usize, Vec<Vec<TokenTree>>)> {
    let mut found = Vec::new();
    let mut attributes: Vec<Vec<TokenTree>> = Vec::new();
    let mut at = 0;
    while let Some(token) = tokens.get(at) {
        match token {
            TokenTree::Punct(hash) if hash.as_char() == '#' => {
                if let Some(TokenTree::Group(attribute)) = tokens.get(at + 1)
                    && attribute.delimiter() == Delimiter::Bracket
                {
                    attributes.push(attribute.stream().into_iter().collect());
                    at += 1;
                }
            }
            TokenTree::Ident(ident) if ident == "pub" => {}
            TokenTree::Group(group)
                if group.delimiter() == Delimiter::Parenthesis
                    && at > 0
                    && matches!(&tokens[at - 1], TokenTree::Ident(i) if i == "pub") => {}
            TokenTree::Ident(keyword) if keyword == "struct" || keyword == "enum" => {
                if let Some(TokenTree::Ident(name)) = tokens.get(at + 1) {
                    found.push((
                        name.to_string(),
                        keyword.span().start().line,
                        std::mem::take(&mut attributes),
                    ));
                }
            }
            TokenTree::Group(group) => {
                attributes.clear();
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                found.extend(types_with_attributes(&inner));
            }
            _ => attributes.clear(),
        }
        at += 1;
    }
    found
}

/// Whether `tokens` hold the identifier `name` at any depth.
pub fn holds(tokens: &[TokenTree], name: &str) -> bool {
    tokens.iter().any(|token| match token {
        TokenTree::Ident(ident) => ident == name,
        TokenTree::Group(group) => {
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            holds(&inner, name)
        }
        _ => false,
    })
}
