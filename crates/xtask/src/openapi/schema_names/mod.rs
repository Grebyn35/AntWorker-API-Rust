//! The schema names that `check-openapi` reads in the code, and the `ToSchema` whose names it
//! can't read, as the comment of [`names`](super::names) says.

use proc_macro2::{TokenStream, TokenTree};

use crate::{Finding, repo::File, rust};

mod elsewhere;
mod impls;

use impls::implemented;

/// The code of `app`, each of whose schema names is a component (API-17).
const APP: &str = "crates/app/src/";

/// A schema name that the check reads in the code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schema {
    /// The type that takes the name.
    pub type_name: String,
    /// The name.
    pub name: String,
    /// The file where the check reads the name.
    pub path: String,
    /// The line where the check reads the name.
    pub line: usize,
    /// Whether a file of `app` writes the type's `ToSchema`, through a derive or an impl.
    pub in_app: bool,
    /// Whether the type is a derive's `struct` or `enum` whose name `<` follows.
    pub generic: bool,
}

/// The schema names of some files, and a finding for each `ToSchema` whose name the check can't
/// read.
#[derive(Default)]
pub struct Reading {
    /// The names, file by file: a file's derives in the order of their lines, then the names of
    /// its impls in theirs, where an impl inside a `macro_rules!` gives those of the macro's
    /// invocations, and one whose name is its type parameter's constant those of the trait's
    /// impls, each of every file in turn.
    pub schemas: Vec<Schema>,
    /// The findings.
    pub refused: Vec<Finding>,
}

/// A file's tokens, and how many of its lines come before its test module.
struct Source<'a> {
    path: &'a str,
    tokens: Vec<TokenTree>,
    before: usize,
}

/// A `macro_rules!`: its name, and the matcher of each of its rules.
struct MacroRules {
    name: String,
    matchers: Vec<Vec<TokenTree>>,
}

/// Reads the schema names of `files`, Rust files of `platform` and `app`.
pub fn read<'a>(files: impl IntoIterator<Item = &'a File>) -> Reading {
    let mut reading = Reading::default();
    let mut sources = Vec::new();
    for file in files {
        let tokens = file.text.parse::<TokenStream>().map_err(syn::Error::from);
        match tokens.and_then(|tokens| Ok((tokens, rust::test_module(&file.text)?))) {
            Ok((tokens, tests)) => sources.push(Source {
                path: &file.path,
                tokens: tokens.into_iter().collect(),
                before: tests.before,
            }),
            Err(error) => reading.refused.push(Finding::new(
                &file.path,
                error.span().start().line,
                format!(
                    "{error}: the check can't read the schema names of a file that doesn't parse \
                     (API-17)"
                ),
            )),
        }
    }
    for source in &sources {
        derived(source, &mut reading);
        implemented(source, &source.tokens, None, &sources, &mut reading);
    }
    reading
}

/// The names that `source`'s derives of `ToSchema` give.
fn derived(source: &Source, reading: &mut Reading) {
    for found in rust::types(&source.tokens) {
        let derives = found.attributes.iter().any(|attribute| {
            matches!(attribute.first(), Some(TokenTree::Ident(ident)) if ident == "derive")
                && rust::holds(attribute, "ToSchema")
        });
        if found.line > source.before || !derives {
            continue;
        }
        if found.name.starts_with('$') {
            reading.refused.push(refusal(
                source.path,
                found.line,
                &found.name,
                "a derive names it by a macro's fragment",
            ));
            continue;
        }
        let name = found
            .attributes
            .iter()
            .find_map(|attribute| renamed(attribute))
            .unwrap_or_else(|| found.name.clone());
        reading.schemas.push(Schema {
            type_name: found.name,
            name,
            path: source.path.to_owned(),
            line: found.line,
            in_app: source.path.starts_with(APP),
            generic: found.generic,
        });
    }
}

/// The name that `attribute`, a `#[schema(as = …)]`, gives a type's schema: the segments of its
/// path, joined by `.`, as utoipa joins them, without its generic arguments.
fn renamed(attribute: &[TokenTree]) -> Option<String> {
    let (Some(TokenTree::Ident(ident)), Some(TokenTree::Group(arguments))) =
        (attribute.first(), attribute.get(1))
    else {
        return None;
    };
    if ident != "schema" {
        return None;
    }
    let tokens: Vec<TokenTree> = arguments.stream().into_iter().collect();
    let start = tokens
        .iter()
        .position(|token| matches!(token, TokenTree::Ident(ident) if ident == "as"))?;
    let segments: Vec<String> = tokens[start + 1..]
        .iter()
        .skip_while(|token| matches!(token, TokenTree::Punct(punct) if punct.as_char() == '='))
        .take_while(|token| {
            !matches!(token, TokenTree::Punct(punct) if matches!(punct.as_char(), ',' | '<'))
        })
        .filter_map(|token| match token {
            TokenTree::Ident(ident) => Some(ident.to_string()),
            _ => None,
        })
        .collect();
    (!segments.is_empty()).then(|| segments.join("."))
}

/// A finding for a `ToSchema` of `type_name` whose name the check can't read, and `why`.
fn refusal(path: &str, line: usize, type_name: &str, why: &str) -> Finding {
    Finding::new(
        path,
        line,
        format!(
            "the check can't read the schema name of `{type_name}`: {why}; write it in one of the \
             forms that the comment of crates/xtask/src/openapi/names.rs lists (API-17)"
        ),
    )
}

fn is_ident(token: Option<&TokenTree>, name: &str) -> bool {
    matches!(token, Some(TokenTree::Ident(ident)) if ident == name)
}

fn line_of(token: &TokenTree) -> usize {
    token.span().start().line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Repo;

    fn read_all(files: &[(&str, &str)]) -> Reading {
        read(Repo::of(files).files())
    }

    #[test]
    fn api_17_the_names_that_derives_and_hand_written_impls_give_are_read() {
        let platform = "macro_rules! id_type {\n    ($(#[$doc:meta])* $name:ident) => {\n        \
                        pub struct $name;\n        impl ToSchema for $name {\n            \
                        fn name() -> Cow<'static, str> {\n                \
                        Cow::Borrowed(stringify!($name))\n            }\n        }\n    };\n}\n\
                        id_type!(\n    /// A user's ID.\n    UserId\n);\n\
                        #[macro_export]\nmacro_rules! filter_values {\n    \
                        ($(#[$doc:meta])* $visibility:vis $name:ident($item:ty)) => {\n        \
                        $visibility struct $name;\n        impl ::utoipa::ToSchema for $name {}\n    \
                        };\n}\n\
                        impl ToSchema for Limit {\n    fn name() -> Cow<'static, str> {\n        \
                        Cow::Borrowed(\"Limit\")\n    }\n}\n\
                        impl ToSchema for Renamed {\n    fn name() -> Cow<'static, str> {\n        \
                        Cow::Borrowed(r\"Other\")\n    }\n}\n\
                        impl ToSchema for Plain {}\n\
                        #[derive(ToSchema)]\n#[schema(as = path::to::Problem, description = \"x\")]\n\
                        struct Body {}\n\
                        #[derive(Serialize, ToSchema)]\n#[schema(title = \"Titled\")]\n\
                        pub struct Thing<T> {}\n\
                        #[derive(ToSchema)]\n#[schema(as = Wrapped<Thing>)]\nstruct Wrapper<T> {}\n";
        let app = "platform::filter_values!(\n    /// The actions.\n    pub(crate) Actions(ActionName)\n);\n\
                   pub(crate) trait LongKind {\n    const SCHEMA: &'static str;\n}\n\
                   impl LongKind for ForReset {\n    const CHARS: usize = 16;\n    \
                   const SCHEMA: &'static str = \"ResetCode\";\n}\n\
                   impl<P: LongKind> ToSchema for LongCode<P> {\n    \
                   fn name() -> Cow<'static, str> {\n        Cow::Borrowed(P::SCHEMA)\n    }\n}\n\
                   #[cfg(test)]\nmod tests {\n    impl LongKind for ForTest {\n        \
                   const SCHEMA: &'static str = \"TestCode\";\n    }\n    id_type!(TestId);\n}\n";
        let reading = read_all(&[
            ("crates/platform/src/types.rs", platform),
            ("crates/app/src/modules/auth/model/codes.rs", app),
        ]);

        let read: Vec<String> = reading
            .schemas
            .iter()
            .map(|schema| {
                format!(
                    "{}:{} {} {} {} {}",
                    schema.path,
                    schema.line,
                    schema.type_name,
                    schema.name,
                    schema.in_app,
                    schema.generic
                )
            })
            .collect();

        assert!(reading.refused.is_empty(), "{:#?}", reading.refused);
        assert_eq!(
            read,
            [
                "crates/platform/src/types.rs:35 Body path.to.Problem false false",
                "crates/platform/src/types.rs:38 Thing Thing false true",
                "crates/platform/src/types.rs:41 Wrapper Wrapped false true",
                "crates/platform/src/types.rs:13 UserId UserId false false",
                "crates/app/src/modules/auth/model/codes.rs:3 Actions Actions false false",
                "crates/platform/src/types.rs:22 Limit Limit false false",
                "crates/platform/src/types.rs:27 Renamed Other false false",
                "crates/platform/src/types.rs:32 Plain Plain false false",
                "crates/app/src/modules/auth/model/codes.rs:10 LongCode ResetCode true false",
            ]
        );
    }

    #[test]
    fn api_17_a_to_schema_whose_name_the_check_can_t_read_is_refused() {
        let model = "impl ToSchema for Owned {\n    fn name() -> Cow<'static, str> {\n        \
                     Cow::Owned(String::new())\n    }\n}\n\
                     macro_rules! typed {\n    ($ty:ty, $name:ident) => {\n        \
                     impl ToSchema for $name {\n            fn name() -> Cow<'static, str> {\n                \
                     Cow::Borrowed(stringify!($name))\n            }\n        }\n    };\n}\n\
                     macro_rules! derived {\n    ($name:ident) => {\n        #[derive(ToSchema)]\n        \
                     struct $name;\n    };\n}\n\
                     impl<P> ToSchema for Unbound<P> {\n    fn name() -> Cow<'static, str> {\n        \
                     Cow::Borrowed(P::SCHEMA)\n    }\n}\n\
                     impl<P: Kind> ToSchema for Bound<P> {\n    fn name() -> Cow<'static, str> {\n        \
                     Cow::Borrowed(P::NAME)\n    }\n}\n\
                     impl Kind for A {\n    const NAME: &'static str = OTHER;\n}\n\
                     macro_rules! plain {\n    ($name:ident) => {\n        impl ToSchema for $name {}\n    \
                     };\n}\n\
                     macro_rules! many {\n    ($($each:ident),*) => {\n        $(plain!($each);)*\n    \
                     };\n}\n\
                     impl ToSchema for (A, B) {}\n";
        let reading = read_all(&[
            ("crates/app/src/modules/things/model.rs", model),
            ("crates/platform/src/broken.rs", "fn a() {}\nfn b( {}\n"),
        ]);

        let mut refused: Vec<String> = reading
            .refused
            .iter()
            .map(|finding| format!("{}:{}", finding.path, finding.line))
            .collect();
        refused.sort();

        assert!(reading.schemas.is_empty(), "{:#?}", reading.schemas);
        assert_eq!(
            refused,
            [
                "crates/app/src/modules/things/model.rs:1",
                "crates/app/src/modules/things/model.rs:18",
                "crates/app/src/modules/things/model.rs:21",
                "crates/app/src/modules/things/model.rs:31",
                "crates/app/src/modules/things/model.rs:41",
                "crates/app/src/modules/things/model.rs:44",
                "crates/app/src/modules/things/model.rs:8",
                "crates/platform/src/broken.rs:2",
            ]
        );
    }
}
