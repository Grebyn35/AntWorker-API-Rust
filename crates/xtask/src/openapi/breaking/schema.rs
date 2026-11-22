//! The comparison of what the last final release's document and the committed one say of one
//! value, by the side that sends it (API-18).
//!
//! A client sends a request's values, so the committed document takes every value that the
//! release's takes: a finding is what it refuses of them. The API sends a response's values, so
//! the committed document says of them at least what the release's says: a finding is what a
//! client that read the release's document may now meet. [`Comparison::compare`] lists each.

use std::{collections::BTreeSet, fmt};

use serde_json::Value;

use super::shape::Shape;

/// Who sends a value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// A client: a parameter or a request's body.
    Request,
    /// The API: a response's body or an item of its stream.
    Response,
}

/// The bounds below which a value isn't taken.
const LOWER: [&str; 5] = [
    "minLength",
    "minimum",
    "exclusiveMinimum",
    "minItems",
    "minProperties",
];

/// The bounds above which a value isn't taken.
const UPPER: [&str; 5] = [
    "maxLength",
    "maximum",
    "exclusiveMaximum",
    "maxItems",
    "maxProperties",
];

/// The keywords that name the form of a string.
const FORMS: [&str; 2] = ["format", "pattern"];

/// Whether `one` allows something that `other` doesn't, where `None` allows everything.
fn outside<T: Ord>(one: Option<&BTreeSet<T>>, other: Option<&BTreeSet<T>>) -> bool {
    match (one, other) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(one), Some(other)) => !one.is_subset(other),
    }
}

/// Where in an operation a value stands: what holds it, and the way to it from there.
#[derive(Clone)]
pub struct Place {
    root: String,
    path: String,
}

impl Place {
    /// What holds a value, such as an operation's body.
    pub fn root(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            path: String::new(),
        }
    }

    fn at(&self, step: impl fmt::Display) -> Self {
        Self {
            root: self.root.clone(),
            path: format!("{}{step}", self.path),
        }
    }
}

impl fmt::Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.path.trim_start_matches('.') {
            "" => f.write_str(&self.root),
            path => write!(f, "{}'s `{path}`", self.root),
        }
    }
}

/// The comparison of the schemas of two documents.
pub struct Comparison<'a> {
    /// The document of the last final release.
    pub release: &'a Value,
    /// The committed document.
    pub current: &'a Value,
    /// What the committed document breaks.
    pub findings: Vec<String>,
    /// The pairs of schemas whose comparison is under way, in this comparison or in the one whose
    /// alternative of a `oneOf` it tries, so that a schema that holds itself, through a property,
    /// an item or an alternative, is compared once.
    comparing: Vec<(Vec<*const Value>, Vec<*const Value>)>,
}

impl<'a> Comparison<'a> {
    /// A comparison of `release`'s schemas with `current`'s.
    pub fn new(release: &'a Value, current: &'a Value) -> Self {
        Self {
            release,
            current,
            findings: Vec::new(),
            comparing: Vec::new(),
        }
    }

    /// Adds a finding for each thing that `current`, the committed document's schemas of the
    /// value at `place`, break of `release`, the release's, read as [`Shape`] reads them.
    ///
    /// A side without a `type` reads as one of any type. On either side:
    /// - a property of the release's that is gone, and each property of both compared in turn,
    ///   as are the `items`, the `contentSchema` and the schemas under `additionalProperties`
    ///   where both have them;
    /// - an alternative of the release's `oneOf` that is gone: one for which the document has
    ///   none of its name that breaks nothing of it, and no other that breaks nothing of it.
    ///   A new alternative is no finding. Alternatives on one side alone are a finding;
    /// - a keyword that [`Shape`] keeps as it is, other than a bound and a string's form, whose
    ///   value differs, or that one side alone has.
    ///
    /// Of a request, what takes less than it took:
    /// - a type, a value of an `enum` or null that the release's takes and it doesn't, an
    ///   `enum` where there was none;
    /// - a property that `required` names now and didn't;
    /// - `additionalProperties` that is `false` now and wasn't, or holds schemas now where it
    ///   held none and wasn't `false`; `items` or a `contentSchema` where there was none;
    /// - a bound that is new or tighter, and a `format` or a `pattern` that is new or another.
    ///
    /// Of a response, what says less than it said:
    /// - a type that it didn't have, or no type where it had one; null where it wasn't;
    /// - an `enum` with other values than the release's, or none where it had one;
    /// - a property that `required` named and doesn't;
    /// - no schema under `additionalProperties`, which isn't `false`, where there were some;
    ///   no `items` or no `contentSchema` where there was one;
    /// - a bound that is gone or looser, and a `format` or a `pattern` that is gone or another.
    pub fn compare(
        &mut self,
        place: &Place,
        side: Side,
        release: &[&'a Value],
        current: &[&'a Value],
    ) {
        let pointers = |schemas: &[&Value]| -> Vec<*const Value> {
            schemas.iter().map(|s| std::ptr::from_ref(*s)).collect()
        };
        let pair = (pointers(release), pointers(current));
        if self.comparing.contains(&pair) {
            return;
        }
        self.comparing.push(pair);
        let shapes = Shape::of(self.release, release)
            .and_then(|release| Ok((release, Shape::of(self.current, current)?)));
        match shapes.and_then(|(release, current)| self.shapes(place, side, &release, &current)) {
            Ok(()) => {}
            Err(unread) => self
                .findings
                .push(format!("{place}: {unread}, which the diff can't read")),
        }
        self.comparing.pop();
    }

    fn shapes(
        &mut self,
        place: &Place,
        side: Side,
        release: &Shape<'a>,
        current: &Shape<'a>,
    ) -> Result<(), String> {
        self.kinds(place, side, release, current);
        self.properties(place, side, release, current);
        self.additional(place, side, release, current);
        self.alternatives(place, side, release, current);
        for (step, of_release, of_current) in [
            ("[]", &release.items, &current.items),
            ("{parsed}", &release.content, &current.content),
        ] {
            match (of_release.is_empty(), of_current.is_empty(), side) {
                (false, false, _) => self.compare(&place.at(step), side, of_release, of_current),
                (true, false, Side::Request) => self
                    .findings
                    .push(format!("{} is limited to a schema now", place.at(step))),
                (false, true, Side::Response) => self
                    .findings
                    .push(format!("{} has no schema now", place.at(step))),
                _ => {}
            }
        }
        self.keywords(place, side, release, current)
    }

    /// The types, null and the `enum`.
    fn kinds(&mut self, place: &Place, side: Side, release: &Shape<'a>, current: &Shape<'a>) {
        let listed =
            |values: &BTreeSet<String>| values.iter().cloned().collect::<Vec<_>>().join(", ");
        let typed = |types: &Option<BTreeSet<&str>>| {
            types.as_ref().map_or_else(
                || "any type".to_owned(),
                |types| types.iter().copied().collect::<Vec<_>>().join(" or "),
            )
        };
        match side {
            Side::Request => {
                if release.nullable && !current.nullable {
                    self.findings.push(format!("{place} no longer takes null"));
                }
                if outside(release.types.as_ref(), current.types.as_ref()) {
                    self.findings.push(format!(
                        "{place} takes {} where it took {}",
                        typed(&current.types),
                        typed(&release.types)
                    ));
                }
                match (&release.values, &current.values) {
                    (Some(taken), Some(takes)) if !taken.is_subset(takes) => {
                        let gone: Vec<String> = taken.difference(takes).cloned().collect();
                        self.findings
                            .push(format!("{place} no longer takes {}", gone.join(", ")));
                    }
                    (None, Some(takes)) => self
                        .findings
                        .push(format!("{place} takes only {} now", listed(takes))),
                    _ => {}
                }
            }
            Side::Response => {
                if current.nullable && !release.nullable {
                    self.findings.push(format!("{place} may be null now"));
                }
                if outside(current.types.as_ref(), release.types.as_ref()) {
                    self.findings.push(format!(
                        "{place} is of {} where it was of {}",
                        typed(&current.types),
                        typed(&release.types)
                    ));
                }
                match (&release.values, &current.values) {
                    (Some(was), Some(is)) if was != is => self.findings.push(format!(
                        "{place} is one of {} where it was one of {}",
                        listed(is),
                        listed(was)
                    )),
                    (Some(was), None) => self.findings.push(format!(
                        "{place} may be any value where it was one of {}",
                        listed(was)
                    )),
                    _ => {}
                }
            }
        }
    }

    fn properties(&mut self, place: &Place, side: Side, release: &Shape<'a>, current: &Shape<'a>) {
        for (name, of_release) in &release.properties {
            let property = place.at(format_args!(".{name}"));
            match current.properties.get(name) {
                Some(of_current) => self.compare(&property, side, of_release, of_current),
                None => self.findings.push(format!("{property} is gone")),
            }
        }
        match side {
            Side::Request => {
                for name in current.required.difference(&release.required) {
                    let new = if release.properties.contains_key(name) {
                        ""
                    } else {
                        "new and "
                    };
                    self.findings.push(format!(
                        "{} is {new}required now",
                        place.at(format_args!(".{name}"))
                    ));
                }
            }
            Side::Response => {
                for name in release.required.difference(&current.required) {
                    if current.properties.contains_key(name) {
                        self.findings.push(format!(
                            "{} may be absent now",
                            place.at(format_args!(".{name}"))
                        ));
                    }
                }
            }
        }
    }

    /// `additionalProperties`.
    fn additional(&mut self, place: &Place, side: Side, release: &Shape<'a>, current: &Shape<'a>) {
        let schemas = |all: &[&'a Value]| -> Vec<&'a Value> {
            all.iter().copied().filter(|one| one.is_object()).collect()
        };
        let closed = |all: &[&Value]| all.iter().any(|one| **one == Value::Bool(false));
        let (of_release, of_current) = (schemas(&release.additional), schemas(&current.additional));
        let others = place.at(".*");
        match (of_release.is_empty(), of_current.is_empty(), side) {
            (false, false, _) => self.compare(&others, side, &of_release, &of_current),
            (true, false, Side::Request) if !closed(&release.additional) => self
                .findings
                .push(format!("{others} is limited to a schema now")),
            (false, true, Side::Response) if !closed(&current.additional) => {
                self.findings.push(format!("{others} has no schema now"));
            }
            _ => {}
        }
        if side == Side::Request && closed(&current.additional) && !closed(&release.additional) {
            self.findings
                .push(format!("{place} takes no other property now"));
        }
    }

    /// The members of a `oneOf`.
    fn alternatives(
        &mut self,
        place: &Place,
        side: Side,
        release: &Shape<'a>,
        current: &Shape<'a>,
    ) {
        if release.alternatives.is_empty() != current.alternatives.is_empty() {
            self.findings.push(format!(
                "{place} has alternatives on one side alone, which the diff can't read"
            ));
            return;
        }
        for (name, of_release) in &release.alternatives {
            let named = current
                .alternatives
                .iter()
                .find(|(other, _)| other == name)
                .map(|(_, schema)| *schema);
            // A trial takes the pairs under way, so that an alternative that holds the schema
            // whose alternative it is ends its comparison there, as a property that does ends.
            let kept = |this: &Self, of_current: &'a Value| {
                let mut trial = Self::new(this.release, this.current);
                trial.comparing.clone_from(&this.comparing);
                trial.compare(place, side, &[*of_release], &[of_current]);
                trial.findings.is_empty()
            };
            let keeps = named.is_some_and(|schema| kept(self, schema))
                || current
                    .alternatives
                    .iter()
                    .any(|(_, schema)| kept(self, schema));
            match (keeps, named) {
                (true, _) => {}
                // What the alternative of the same name breaks says more than that it is gone.
                (false, Some(schema)) => self.compare(
                    &place.at(format_args!("<{name}>")),
                    side,
                    &[*of_release],
                    &[schema],
                ),
                (false, None) => self
                    .findings
                    .push(format!("{place}: the alternative `{name}` is gone")),
            }
        }
    }

    /// The bounds, a string's form, and every keyword that [`Shape`] keeps as it is.
    fn keywords(
        &mut self,
        place: &Place,
        side: Side,
        release: &Shape<'a>,
        current: &Shape<'a>,
    ) -> Result<(), String> {
        let keys: BTreeSet<&str> = release
            .others
            .keys()
            .chain(current.others.keys())
            .copied()
            .collect();
        for key in keys {
            let (was, is) = (release.other(key)?, current.other(key)?);
            if was == is {
                continue;
            }
            // The side whose value may go, or loosen, without a finding: a request's own, and
            // for a response the release's.
            let (loose, tight) = match side {
                Side::Request => (is, was),
                Side::Response => (was, is),
            };
            let numbers = loose
                .and_then(Value::as_f64)
                .zip(tight.and_then(Value::as_f64));
            let fine = if LOWER.contains(&key) {
                loose.is_none() || numbers.is_some_and(|(loose, tight)| loose <= tight)
            } else if UPPER.contains(&key) {
                loose.is_none() || numbers.is_some_and(|(loose, tight)| loose >= tight)
            } else {
                FORMS.contains(&key) && loose.is_none()
            };
            if !fine {
                let shown = |value: Option<&Value>| {
                    value.map_or_else(|| "unset".to_owned(), Value::to_string)
                };
                self.findings.push(format!(
                    "{place}: `{key}` is {} where it was {}",
                    shown(is),
                    shown(was)
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// What the comparison finds of `current` against `release`, each the schema of a value
    /// named `it`, with no schema that either refers to.
    fn found(side: Side, release: &Value, current: &Value) -> Vec<String> {
        let document = json!({});
        let mut comparison = Comparison::new(&document, &document);
        comparison.compare(&Place::root("it"), side, &[release], &[current]);
        comparison.findings
    }

    /// What it finds of a request, then of a response.
    fn both(release: &Value, current: &Value) -> (Vec<String>, Vec<String>) {
        (
            found(Side::Request, release, current),
            found(Side::Response, release, current),
        )
    }

    fn none() -> Vec<String> {
        Vec::new()
    }

    #[test]
    fn api_18_a_schema_that_says_the_same_breaks_nothing() {
        let schema = json!({
            "type": "object",
            "required": ["id"],
            "additionalProperties": false,
            "properties": {
                "id": {"type": "string", "format": "uuid"},
                "names": {"type": "array", "items": {"type": "string", "maxLength": 9}, "maxItems": 3},
                "role": {"enum": ["owner", "member"], "type": ["string", "null"]}
            }
        });

        assert_eq!(both(&schema, &schema), (none(), none()));
    }

    #[test]
    fn api_18_a_request_keeps_the_types_and_null_that_it_took() {
        let (text, either, nullable, any) = (
            json!({"type": "string"}),
            json!({"type": ["string", "integer"]}),
            json!({"type": ["string", "null"]}),
            json!({}),
        );

        assert_eq!(found(Side::Request, &text, &either), none());
        assert_eq!(found(Side::Request, &text, &nullable), none());
        assert_eq!(found(Side::Request, &text, &any), none());
        assert_eq!(
            found(Side::Request, &either, &text),
            ["it takes string where it took integer or string"]
        );
        assert_eq!(
            found(Side::Request, &any, &text),
            ["it takes string where it took any type"]
        );
        assert_eq!(
            found(Side::Request, &nullable, &text),
            ["it no longer takes null"]
        );
    }

    #[test]
    fn api_18_a_response_gains_no_type_and_no_null() {
        let (text, either, nullable, any) = (
            json!({"type": "string"}),
            json!({"type": ["string", "integer"]}),
            json!({"oneOf": [{"type": "null"}, {"type": "string"}]}),
            json!({}),
        );

        assert_eq!(found(Side::Response, &either, &text), none());
        assert_eq!(found(Side::Response, &nullable, &text), none());
        assert_eq!(found(Side::Response, &any, &text), none());
        assert_eq!(
            found(Side::Response, &text, &either),
            ["it is of integer or string where it was of string"]
        );
        assert_eq!(
            found(Side::Response, &text, &any),
            ["it is of any type where it was of string"]
        );
        assert_eq!(
            found(Side::Response, &text, &nullable),
            ["it may be null now"]
        );
    }

    #[test]
    fn api_18_a_request_s_enum_may_grow_and_a_response_s_stays() {
        let (two, three, open) = (
            json!({"type": "string", "enum": ["a", "b"]}),
            json!({"type": "string", "enum": ["a", "b", "c"]}),
            json!({"type": "string"}),
        );

        assert_eq!(found(Side::Request, &two, &three), none());
        assert_eq!(found(Side::Request, &two, &open), none());
        assert_eq!(
            found(Side::Request, &three, &two),
            ["it no longer takes \"c\""]
        );
        assert_eq!(
            found(Side::Request, &open, &two),
            ["it takes only \"a\", \"b\" now"]
        );
        assert_eq!(
            found(Side::Response, &two, &three),
            ["it is one of \"a\", \"b\", \"c\" where it was one of \"a\", \"b\""]
        );
        assert_eq!(
            found(Side::Response, &three, &two),
            ["it is one of \"a\", \"b\" where it was one of \"a\", \"b\", \"c\""]
        );
        assert_eq!(
            found(Side::Response, &two, &open),
            ["it may be any value where it was one of \"a\", \"b\""]
        );
        assert_eq!(found(Side::Response, &open, &two), none());
    }

    #[test]
    fn api_18_a_property_stays_and_a_request_s_new_one_is_optional() {
        let release = json!({"properties": {"a": {"type": "string"}, "b": {"type": "string"}}, "required": ["a"]});
        let without = json!({"properties": {"a": {"type": "string"}}, "required": ["a"]});
        let with_optional = json!({"properties": {"a": {}, "b": {}, "c": {}}, "required": ["a"]});
        let with_required =
            json!({"properties": {"a": {}, "b": {}, "c": {}}, "required": ["a", "b", "c"]});
        let none_required = json!({"properties": {"a": {}, "b": {}}});

        assert_eq!(
            both(&release, &without),
            (
                vec!["it's `b` is gone".to_owned()],
                vec!["it's `b` is gone".to_owned()]
            )
        );
        assert_eq!(found(Side::Request, &release, &with_optional), none());
        assert_eq!(
            found(Side::Request, &release, &with_required),
            [
                "it's `b` is required now",
                "it's `c` is new and required now"
            ]
        );
        assert_eq!(found(Side::Request, &release, &none_required), none());
        // A response's new property, required or not, is growth; one that was required stays so.
        assert_eq!(
            found(
                Side::Response,
                &release,
                &json!({"properties": {"a": {"type": "string"}, "b": {"type": "string"}, "c": {}}, "required": ["a", "b", "c"]})
            ),
            none()
        );
        assert_eq!(
            found(
                Side::Response,
                &release,
                &json!({"properties": {"a": {"type": "string"}, "b": {"type": "string"}}})
            ),
            ["it's `a` may be absent now"]
        );
    }

    #[test]
    fn api_18_a_nested_value_is_compared_where_it_stands() {
        let release = json!({"properties": {"data": {"type": "array", "items": {
            "properties": {"id": {"type": "string"}}
        }}}});
        let current = json!({"properties": {"data": {"type": "array", "items": {
            "properties": {"id": {"type": "integer"}}
        }}}});

        assert_eq!(
            found(Side::Response, &release, &current),
            ["it's `data[].id` is of integer where it was of string"]
        );
        let frame = json!({"type": "string", "contentSchema": {"properties": {"trace_id": {"type": "string"}}}});
        let gone = json!({"type": "string", "contentSchema": {"properties": {}}});
        assert_eq!(
            found(Side::Response, &frame, &gone),
            ["it's `{parsed}.trace_id` is gone"]
        );
        assert_eq!(
            found(Side::Response, &frame, &json!({"type": "string"})),
            ["it's `{parsed}` has no schema now"]
        );
        assert_eq!(
            found(
                Side::Request,
                &json!({"type": "array"}),
                &json!({"type": "array", "items": {"type": "string"}})
            ),
            ["it's `[]` is limited to a schema now"]
        );
        assert_eq!(
            found(
                Side::Request,
                &json!({"type": "array", "items": {"type": "string"}}),
                &json!({"type": "array"})
            ),
            none()
        );
    }

    #[test]
    fn api_18_a_request_s_other_properties_stay_taken() {
        let (open, closed, typed) = (
            json!({"type": "object"}),
            json!({"type": "object", "additionalProperties": false}),
            json!({"type": "object", "additionalProperties": {"type": "string"}}),
        );

        assert_eq!(found(Side::Request, &closed, &open), none());
        assert_eq!(found(Side::Request, &closed, &typed), none());
        assert_eq!(found(Side::Request, &typed, &open), none());
        assert_eq!(
            found(Side::Request, &open, &closed),
            ["it takes no other property now"]
        );
        assert_eq!(
            found(Side::Request, &typed, &closed),
            ["it takes no other property now"]
        );
        assert_eq!(
            found(Side::Request, &open, &typed),
            ["it's `*` is limited to a schema now"]
        );
        assert_eq!(
            found(
                Side::Request,
                &typed,
                &json!({"additionalProperties": {"type": "integer"}})
            ),
            ["it's `*` takes integer where it took string"]
        );
        assert_eq!(
            found(Side::Response, &typed, &open),
            ["it's `*` has no schema now"]
        );
        assert_eq!(found(Side::Response, &typed, &closed), none());
        assert_eq!(found(Side::Response, &open, &closed), none());
        assert_eq!(found(Side::Response, &closed, &open), none());
    }

    #[test]
    fn api_18_a_request_s_bound_may_loosen_and_a_response_s_tighten() {
        let bounded =
            |min: u64, max: u64| json!({"type": "string", "minLength": min, "maxLength": max});
        let unbounded = json!({"type": "string"});

        assert_eq!(
            found(Side::Request, &bounded(2, 9), &bounded(1, 10)),
            none()
        );
        assert_eq!(found(Side::Request, &bounded(2, 9), &unbounded), none());
        assert_eq!(
            found(Side::Request, &bounded(2, 9), &bounded(3, 8)),
            [
                "it: `maxLength` is 8 where it was 9",
                "it: `minLength` is 3 where it was 2"
            ]
        );
        assert_eq!(
            found(Side::Request, &unbounded, &bounded(0, 9)),
            [
                "it: `maxLength` is 9 where it was unset",
                "it: `minLength` is 0 where it was unset"
            ]
        );
        assert_eq!(
            found(Side::Response, &bounded(2, 9), &bounded(3, 8)),
            none()
        );
        assert_eq!(found(Side::Response, &unbounded, &bounded(3, 8)), none());
        assert_eq!(
            found(Side::Response, &bounded(2, 9), &bounded(1, 10)),
            [
                "it: `maxLength` is 10 where it was 9",
                "it: `minLength` is 1 where it was 2"
            ]
        );
        assert_eq!(
            found(Side::Response, &bounded(2, 9), &unbounded),
            [
                "it: `maxLength` is unset where it was 9",
                "it: `minLength` is unset where it was 2"
            ]
        );
    }

    #[test]
    fn api_18_a_string_s_form_and_every_other_keyword_stay() {
        let (uuid, date, plain) = (
            json!({"type": "string", "format": "uuid"}),
            json!({"type": "string", "format": "date"}),
            json!({"type": "string"}),
        );

        assert_eq!(found(Side::Request, &uuid, &plain), none());
        assert_eq!(found(Side::Response, &plain, &uuid), none());
        assert_eq!(
            both(&uuid, &date),
            (
                vec!["it: `format` is \"date\" where it was \"uuid\"".to_owned()],
                vec!["it: `format` is \"date\" where it was \"uuid\"".to_owned()]
            )
        );
        assert_eq!(
            found(Side::Request, &plain, &uuid),
            ["it: `format` is \"uuid\" where it was unset"]
        );
        assert_eq!(
            found(Side::Response, &uuid, &plain),
            ["it: `format` is unset where it was \"uuid\""]
        );
        // A keyword that the diff knows nothing of stays as it is, on both sides.
        let (fifty, twenty) = (json!({"default": 50}), json!({"default": 20}));
        for (release, current, finding) in [
            (&fifty, &twenty, "it: `default` is 20 where it was 50"),
            (&fifty, &json!({}), "it: `default` is unset where it was 50"),
            (&json!({}), &fifty, "it: `default` is 50 where it was unset"),
            (
                &json!({"x-event": "member.added"}),
                &json!({"x-event": "member.joined"}),
                "it: `x-event` is \"member.joined\" where it was \"member.added\"",
            ),
        ] {
            assert_eq!(
                both(release, current),
                (vec![finding.to_owned()], vec![finding.to_owned()])
            );
        }
        // A description is no such keyword.
        assert_eq!(
            both(
                &json!({"description": "a"}),
                &json!({"description": "b", "title": "c"})
            ),
            (none(), none())
        );
    }

    #[test]
    fn api_18_an_alternative_stays_and_a_new_one_is_growth() {
        let document = json!({"components": {"schemas": {
            "Added": {"properties": {"event": {"enum": ["member.added"]}, "id": {"type": "string"}}, "required": ["event", "id"]},
            "Removed": {"properties": {"event": {"enum": ["member.removed"]}}, "required": ["event"]},
            "Joined": {"properties": {"event": {"enum": ["member.joined"]}, "id": {"type": "string"}}, "required": ["event", "id"]},
            "Renamed": {"properties": {"event": {"enum": ["member.removed"]}, "at": {"type": "string"}}, "required": ["event"]},
            "AddedWithoutId": {"properties": {"event": {"enum": ["member.added"]}}, "required": ["event"]}
        }}});
        let frames = |names: &[&str]| json!({"oneOf": names.iter().map(|name| json!({"$ref": format!("#/components/schemas/{name}")})).collect::<Vec<_>>()});
        let found = |side: Side, release: &[&str], current: &[&str]| {
            let (release, current) = (frames(release), frames(current));
            let mut comparison = Comparison::new(&document, &document);
            comparison.compare(&Place::root("it"), side, &[&release], &[&current]);
            comparison.findings
        };

        for side in [Side::Request, Side::Response] {
            assert_eq!(
                found(side, &["Added", "Removed"], &["Removed", "Joined", "Added"]),
                none()
            );
            // A schema under another name that says the same keeps the alternative.
            assert_eq!(
                found(side, &["Added", "Removed"], &["Added", "Renamed"]),
                none()
            );
            assert_eq!(
                found(side, &["Added", "Removed"], &["Joined", "Removed"]),
                ["it: the alternative `Added` is gone"]
            );
        }
        assert_eq!(
            found(Side::Response, &["Added", "Removed"], &["Removed"]),
            ["it has alternatives on one side alone, which the diff can't read"]
        );
        // Under its own name, what the alternative breaks is named.
        let mut changed = document.clone();
        changed["components"]["schemas"]["Added"] =
            document["components"]["schemas"]["AddedWithoutId"].clone();
        let (release, current) = (frames(&["Added", "Removed"]), frames(&["Added", "Removed"]));
        let mut comparison = Comparison::new(&document, &changed);
        comparison.compare(&Place::root("it"), Side::Response, &[&release], &[&current]);
        assert_eq!(comparison.findings, ["it's `<Added>.id` is gone"]);
    }

    #[test]
    fn api_18_what_the_diff_can_t_read_is_a_finding() {
        assert_eq!(
            both(&json!({"type": "string"}), &json!({"type": 3})),
            (
                vec!["it: the type `3` is no name or names, which the diff can't read".to_owned()],
                vec!["it: the type `3` is no name or names, which the diff can't read".to_owned()]
            )
        );
        let twice = json!({"allOf": [{"maxLength": 3}, {"maxLength": 4}]});
        assert_eq!(
            found(Side::Request, &json!({"maxLength": 3}), &twice),
            ["it: two schemas of one value give it a `maxLength` each, which the diff can't read"]
        );
        let two = json!({"oneOf": [{"type": "string"}, {"type": "integer"}]});
        assert_eq!(
            found(Side::Request, &json!({}), &two),
            ["it has alternatives on one side alone, which the diff can't read"]
        );
    }

    #[test]
    fn api_18_a_schema_that_holds_itself_is_compared_once() {
        let document = json!({"components": {"schemas": {
            "Tree": {"properties": {"name": {"type": "string"}, "children": {"type": "array", "items": {"$ref": "#/components/schemas/Tree"}}}}
        }}});
        let mut renamed = document.clone();
        renamed["components"]["schemas"]["Tree"]["properties"]["name"]["type"] = json!("integer");
        let tree = json!({"$ref": "#/components/schemas/Tree"});

        let mut comparison = Comparison::new(&document, &renamed);
        comparison.compare(&Place::root("it"), Side::Response, &[&tree], &[&tree]);

        assert_eq!(
            comparison.findings,
            [
                "it's `children[].name` is of integer where it was of string",
                "it's `name` is of integer where it was of string"
            ]
        );
    }

    #[test]
    fn api_18_a_schema_that_holds_itself_through_an_alternative_is_compared_once() {
        let document = json!({"components": {"schemas": {
            "Node": {"oneOf": [{"$ref": "#/components/schemas/Leaf"}, {"$ref": "#/components/schemas/Branch"}]},
            "Leaf": {"properties": {"name": {"type": "string"}}, "required": ["name"]},
            "Branch": {"properties": {"children": {"type": "array", "items": {"$ref": "#/components/schemas/Node"}}}, "required": ["children"]}
        }}});
        let mut renamed = document.clone();
        renamed["components"]["schemas"]["Leaf"]["properties"]["name"]["type"] = json!("integer");
        let node = json!({"$ref": "#/components/schemas/Node"});

        let mut same = Comparison::new(&document, &document);
        same.compare(&Place::root("it"), Side::Response, &[&node], &[&node]);
        let mut comparison = Comparison::new(&document, &renamed);
        comparison.compare(&Place::root("it"), Side::Response, &[&node], &[&node]);

        assert_eq!(same.findings, none());
        assert_eq!(
            comparison.findings,
            [
                "it's `<Leaf>.name` is of integer where it was of string",
                "it's `<Branch>.children[]<Leaf>.name` is of integer where it was of string"
            ]
        );
    }
}
