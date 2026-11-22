//! The items of Rust source read as token streams: functions' signatures, the items a file
//! declares at its top level, the fields public types expose, and the attributes of structs and
//! enums.

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
            TokenTree::Ident(keyword) if keyword == "fn" => found.push(signature(tokens, at)),
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                found.extend(signatures(&inner));
            }
            _ => {}
        }
    }
    found
}

/// The signature of the function whose `fn` is the token at `at` of `tokens`.
fn signature(tokens: &[TokenTree], at: usize) -> Signature {
    let end = tokens[at..]
        .iter()
        .position(|token| match token {
            TokenTree::Group(group) => group.delimiter() == Delimiter::Brace,
            TokenTree::Punct(semicolon) => semicolon.as_char() == ';',
            _ => false,
        })
        .map_or(tokens.len(), |end| at + end);
    let line = match &tokens[at] {
        TokenTree::Ident(keyword) => keyword.span().start().line,
        _ => 0,
    };
    Signature {
        public: is_public(&tokens[..at]),
        line,
        tokens: tokens[at + 1..end].to_vec(),
    }
}

/// The keywords of the types an item declares: a struct, an enum, a union, a type alias or a
/// trait.
pub const TYPES: [&str; 5] = ["struct", "enum", "union", "type", "trait"];

/// An item that a file declares at its top level.
pub struct Item {
    /// Its keyword: `fn`, `const`, `static` or one of [`TYPES`].
    pub keyword: String,
    /// Its name.
    pub name: String,
    /// Whether it is `pub`, with or without a restriction.
    pub public: bool,
    /// A function's signature.
    pub signature: Option<Signature>,
}

/// Each function, constant, static and type that `tokens` declare at their top level: a keyword
/// of an item, followed by its name, outside every group, so neither a method nor an item of an
/// inline `mod` is one.
pub fn items(tokens: &[TokenTree]) -> Vec<Item> {
    let mut found = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let TokenTree::Ident(keyword) = token else {
            continue;
        };
        let keyword = keyword.to_string();
        if keyword != "fn"
            && keyword != "const"
            && keyword != "static"
            && !TYPES.contains(&keyword.as_str())
        {
            continue;
        }
        // `static mut NAME`; and `const fn`, whose `fn` declares the function.
        let named = match tokens.get(at + 1) {
            Some(TokenTree::Ident(word)) if word == "mut" => tokens.get(at + 2),
            next => next,
        };
        let Some(TokenTree::Ident(name)) = named else {
            continue;
        };
        if keyword == "const"
            && ["fn", "unsafe", "async", "extern"].contains(&name.to_string().as_str())
        {
            continue;
        }
        found.push(Item {
            name: name.to_string(),
            public: is_public(&tokens[..at]),
            signature: (keyword == "fn").then(|| signature(tokens, at)),
            keyword,
        });
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

/// A struct or an enum, read from tokens.
pub struct Type {
    /// Its name, or `$` and the name of the fragment that a `macro_rules!` names it by.
    pub name: String,
    /// The line of its keyword.
    pub line: usize,
    /// The attributes written before it, each the tokens between its brackets.
    pub attributes: Vec<Vec<TokenTree>>,
    /// Whether `<` follows its name, which opens its generic parameters.
    pub generic: bool,
}

/// Each struct and enum among `tokens`, at any depth, with its name, its line, and the
/// attributes written before it, but one that a macro's fragment names.
pub fn types_with_attributes(tokens: &[TokenTree]) -> Vec<(String, usize, Vec<Vec<TokenTree>>)> {
    types(tokens)
        .into_iter()
        .filter(|found| !found.name.starts_with('$'))
        .map(|found| (found.name, found.line, found.attributes))
        .collect()
}

/// Each struct and enum among `tokens`, at any depth, those that a macro's fragment names among
/// them.
pub fn types(tokens: &[TokenTree]) -> Vec<Type> {
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
                let named = match (tokens.get(at + 1), tokens.get(at + 2)) {
                    (Some(TokenTree::Ident(name)), _) => Some((name.to_string(), at + 2)),
                    (Some(TokenTree::Punct(dollar)), Some(TokenTree::Ident(name)))
                        if dollar.as_char() == '$' =>
                    {
                        Some((format!("${name}"), at + 3))
                    }
                    _ => None,
                };
                if let Some((name, after)) = named {
                    found.push(Type {
                        name,
                        line: keyword.span().start().line,
                        attributes: std::mem::take(&mut attributes),
                        generic: matches!(
                            tokens.get(after),
                            Some(TokenTree::Punct(open)) if open.as_char() == '<'
                        ),
                    });
                }
            }
            TokenTree::Group(group) => {
                attributes.clear();
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                found.extend(types(&inner));
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
