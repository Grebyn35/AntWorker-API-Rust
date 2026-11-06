//! The names a new module's files take: the module's and its resource's, each in the forms its
//! code and documents write them, which replace the templates' own.

/// The module the templates are written for, whose names every template holds.
const TEMPLATE_MODULE: &str = "sample_items";

/// The resource the templates are written for.
const TEMPLATE_RESOURCE: &str = "sample_item";

/// Rust's keywords, strict and reserved, which no module or resource of one word can be named.
const KEYWORDS: [&str; 51] = [
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
    "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof",
    "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// A new module's names: the module's, plural, such as `order_items`, and its resource's,
/// singular, such as `order_item`, each in `snake_case`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    /// The module's name, which its directory, its table and its permissions take.
    pub module: String,
    /// The name of one of its rows.
    pub resource: String,
}

impl Names {
    /// The names of the module `module`, with the resource `resource`, or, without one, the
    /// singular of `module` that English's regular plurals give, or why they can't be.
    pub fn new(module: &str, resource: Option<&str>) -> Result<Self, String> {
        check_name("module", module)?;
        let resource = match resource {
            Some(resource) => resource.to_owned(),
            None => singular(module).ok_or_else(|| {
                format!(
                    "`{module}` doesn't end as a regular English plural does: name the resource \
                     too, such as `just new-module {module} <resource>`"
                )
            })?,
        };
        check_name("resource", &resource)?;
        if resource == module {
            return Err(format!(
                "the module and its resource are both `{module}`: a module is named in the plural, \
                 and its resource in the singular"
            ));
        }
        Ok(Self {
            module: module.to_owned(),
            resource,
        })
    }

    /// The names the templates are written for.
    pub fn template() -> Self {
        Self {
            module: TEMPLATE_MODULE.to_owned(),
            resource: TEMPLATE_RESOURCE.to_owned(),
        }
    }

    /// `text`, a template, with each of the template's names in each form replaced by this
    /// module's in the same form, where it starts a word, or a part of a name in PascalCase. One pass replaces them, the longest
    /// first wherever two start at the same place, so a name that holds a template's name is
    /// never replaced again.
    pub fn apply(&self, text: &str) -> String {
        let template = Self::template();
        let pairs: Vec<(String, String)> = template.forms().into_iter().zip(self.forms()).collect();
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        let mut previous = ' ';
        'scan: while let Some(c) = rest.chars().next() {
            for (from, to) in &pairs {
                // A name in PascalCase also starts a word after a letter, as in `NewSampleItem`.
                let starts_word = !previous.is_ascii_alphanumeric()
                    || (previous.is_ascii_alphabetic() && from.starts_with(char::is_uppercase));
                if starts_word && let Some(after) = rest.strip_prefix(from.as_str()) {
                    out.push_str(to);
                    previous = from.chars().last().unwrap_or(' ');
                    rest = after;
                    continue 'scan;
                }
            }
            out.push(c);
            previous = c;
            rest = &rest[c.len_utf8()..];
        }
        out
    }

    /// Each form of both names, in the order `apply` tries them: the resource after an
    /// indefinite article, then each form of the module before the same form of the resource,
    /// whose name the module's starts with.
    fn forms(&self) -> Vec<String> {
        let resource_words = words(&self.resource);
        let article = article(&resource_words);
        vec![
            format!("{} {resource_words}", capitalized(article)),
            format!("{article} {resource_words}"),
            self.module.to_ascii_uppercase(),
            self.resource.to_ascii_uppercase(),
            pascal(&self.module),
            pascal(&self.resource),
            self.module.clone(),
            self.resource.clone(),
            self.module.replace('_', "-"),
            self.resource.replace('_', "-"),
            capitalized(&words(&self.module)),
            capitalized(&resource_words),
            words(&self.module),
            resource_words,
        ]
    }
}

/// Fails unless `name` is `snake_case` words, each starting with a letter, and no keyword of
/// Rust.
fn check_name(what: &str, name: &str) -> Result<(), String> {
    let words_ok = !name.is_empty()
        && name.split('_').all(|word| {
            word.starts_with(|c: char| c.is_ascii_lowercase())
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
    if !words_ok {
        return Err(format!(
            "the {what} `{name}` isn't snake_case: lowercase words of letters and digits, each \
             starting with a letter, joined by underscores"
        ));
    }
    if KEYWORDS.contains(&name) {
        return Err(format!("the {what} `{name}` is a keyword of Rust"));
    }
    Ok(())
}

/// The singular of `plural` by English's regular plurals, such as `order_item` for
/// `order_items`, `category` for `categories` and `address` for `addresses`, or `None` when it
/// doesn't end as one does.
fn singular(plural: &str) -> Option<String> {
    if let Some(stem) = plural.strip_suffix("ies") {
        return Some(format!("{stem}y"));
    }
    for ending in ["sses", "shes", "ches", "xes", "zes"] {
        if plural.ends_with(ending) {
            return Some(plural[..plural.len() - 2].to_owned());
        }
    }
    plural
        .strip_suffix('s')
        .filter(|stem| !stem.ends_with('s') && !stem.is_empty() && !stem.ends_with('_'))
        .map(str::to_owned)
}

/// `name` in words, as prose writes it, such as `order items`.
fn words(name: &str) -> String {
    name.replace('_', " ")
}

/// `name` in `PascalCase`, such as `OrderItems`.
fn pascal(name: &str) -> String {
    name.split('_').map(capitalized).collect()
}

/// `text` with its first letter in capitals.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_ascii_uppercase().to_string() + chars.as_str()
    })
}

/// The indefinite article before `words`: "an" before a vowel's letter but `u`, whose words
/// mostly start with a consonant's sound, as "user" does, and "a" otherwise.
fn article(words: &str) -> &'static str {
    if words.starts_with(['a', 'e', 'i', 'o']) {
        "an"
    } else {
        "a"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_03_the_resource_is_the_module_s_regular_singular_unless_named() {
        let resource = |module| Names::new(module, None).map(|names| names.resource);
        assert_eq!(resource("order_items").unwrap(), "order_item");
        assert_eq!(resource("categories").unwrap(), "category");
        assert_eq!(resource("addresses").unwrap(), "address");
        assert_eq!(resource("inboxes").unwrap(), "inbox");
        assert!(resource("staff").unwrap_err().contains("doesn't end"));
        assert!(resource("class").unwrap_err().contains("doesn't end"));
        let people = Names::new("people", Some("person")).unwrap();
        assert_eq!(people.resource, "person");
    }

    #[test]
    fn arch_03_a_name_is_snake_case_and_no_keyword() {
        assert!(
            Names::new("Orders", None)
                .unwrap_err()
                .contains("snake_case")
        );
        assert!(
            Names::new("order__items", None)
                .unwrap_err()
                .contains("snake_case")
        );
        assert!(
            Names::new("2fa_codes", None)
                .unwrap_err()
                .contains("snake_case")
        );
        assert!(
            Names::new("types", Some("type"))
                .unwrap_err()
                .contains("keyword")
        );
        assert!(
            Names::new("items", Some("items"))
                .unwrap_err()
                .contains("both")
        );
    }

    #[test]
    fn arch_03_every_form_of_the_template_s_names_becomes_the_module_s() {
        let names = Names::new("order_items", None).unwrap();
        let template = "A sample item, a sample item's name: SAMPLE_ITEMS-001, SAMPLE_ITEM, \
                        SampleItemsRead, SampleItem, sample_items.read, sample_item_id, \
                        /sample-items/{sample_item_id}, Sample items, Sample item not found, \
                        the sample items spec, one sample item.";

        assert_eq!(
            names.apply(template),
            "An order item, an order item's name: ORDER_ITEMS-001, ORDER_ITEM, \
             OrderItemsRead, OrderItem, order_items.read, order_item_id, \
             /order-items/{order_item_id}, Order items, Order item not found, \
             the order items spec, one order item."
        );
    }

    #[test]
    fn arch_03_a_name_that_holds_the_template_s_is_replaced_once() {
        let names = Names::new("sample_item_notes", None).unwrap();
        assert_eq!(
            names.apply("sample_items and a sample item"),
            "sample_item_notes and a sample item note"
        );
        assert_eq!(names.apply("resample_items"), "resample_items");
        assert_eq!(names.apply("NewSampleItem"), "NewSampleItemNote");
        assert_eq!(Names::template().apply("SampleItem"), "SampleItem");
    }
}
