//! What a schema of the document says of a value, read through its references and its `allOf`,
//! as the breaking-change diff compares it.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// The keywords that say nothing of what a value may be.
const ANNOTATIONS: [&str; 6] = [
    "description",
    "title",
    "summary",
    "example",
    "examples",
    "deprecated",
];

/// What schemas that all hold of one value say of it together.
///
/// It reads these keywords of each schema: `$ref`, whose target it reads in the schema's place,
/// beside the schema's other keywords; `allOf`, each of whose members it reads likewise;
/// `oneOf`; `type`; `properties`; `required`; `additionalProperties`; `items`; `enum`; and
/// `contentSchema`. It passes over the keywords of [`ANNOTATIONS`], and keeps every other
/// keyword with the values the schemas give it.
#[derive(Default)]
pub struct Shape<'a> {
    /// Whether the value may be null: a `type` names `null`, or a member of a `oneOf` has the
    /// type `null`.
    pub nullable: bool,
    /// The types but `null` that every `type` names, or `None` where no schema has a `type`
    /// that names another.
    pub types: Option<BTreeSet<&'a str>>,
    /// Each property, with the schemas that hold of it.
    pub properties: BTreeMap<&'a str, Vec<&'a Value>>,
    /// The properties that a `required` names.
    pub required: BTreeSet<&'a str>,
    /// Each `additionalProperties`.
    pub additional: Vec<&'a Value>,
    /// Each `items`.
    pub items: Vec<&'a Value>,
    /// The values that every `enum` lists, as JSON, or `None` without an `enum`.
    pub values: Option<BTreeSet<String>>,
    /// The members of a `oneOf` but those of the type `null`, where they are two or more, each
    /// under the name of the schema that it refers to, or its place in the list. A single one
    /// is read as a schema that holds of the value.
    pub alternatives: Vec<(String, &'a Value)>,
    /// Each `contentSchema`: the schema of what a string holds once it is parsed.
    pub content: Vec<&'a Value>,
    /// Every other keyword, with each value that a schema gives it, once.
    pub others: BTreeMap<&'a str, Vec<&'a Value>>,
}

impl<'a> Shape<'a> {
    /// The shape that `schemas`, of `document`, give one value together, or what it can't read:
    /// a schema that is neither an object nor `true`, which it reads as one that says nothing of
    /// the value, a reference that isn't one to a schema that `document` holds, a `type` that is
    /// no name or list of names, or a second `oneOf` of two or more members that aren't null.
    pub fn of(document: &'a Value, schemas: &[&'a Value]) -> Result<Self, String> {
        let mut shape = Self::default();
        let mut waiting: Vec<&'a Value> = schemas.to_vec();
        let mut read: Vec<*const Value> = Vec::new();
        while let Some(schema) = waiting.pop() {
            if read.contains(&std::ptr::from_ref(schema)) {
                continue;
            }
            read.push(std::ptr::from_ref(schema));
            let members = match schema {
                Value::Bool(true) => continue,
                Value::Object(members) => members,
                other => return Err(format!("the schema `{other}` is no object")),
            };
            for (key, value) in members {
                shape.take(document, key, value, &mut waiting)?;
            }
        }
        Ok(shape)
    }

    /// Reads the keyword `key` of a schema, and puts the schemas that it names for the same
    /// value into `waiting`.
    fn take(
        &mut self,
        document: &'a Value,
        key: &'a str,
        value: &'a Value,
        waiting: &mut Vec<&'a Value>,
    ) -> Result<(), String> {
        match key {
            "$ref" => waiting.push(referred(document, value)?),
            "allOf" => waiting.extend(value.as_array().into_iter().flatten()),
            "oneOf" => {
                let (null, other): (Vec<&Value>, Vec<&Value>) = value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .partition(|member| member["type"] == "null");
                self.nullable |= !null.is_empty();
                match other.as_slice() {
                    [] => {}
                    [one] => waiting.push(one),
                    _ if !self.alternatives.is_empty() => {
                        return Err("two `oneOf` hold of one value".to_owned());
                    }
                    several => {
                        self.alternatives = several
                            .iter()
                            .enumerate()
                            .map(|(index, member)| (alternative(index, member), *member))
                            .collect();
                    }
                }
            }
            "type" => {
                let names: Option<BTreeSet<&str>> = match value {
                    Value::String(name) => Some(BTreeSet::from([name.as_str()])),
                    Value::Array(names) => names.iter().map(Value::as_str).collect(),
                    _ => None,
                };
                let mut names = names.ok_or(format!("the type `{value}` is no name or names"))?;
                self.nullable |= names.remove("null");
                if !names.is_empty() {
                    self.types = Some(match self.types.take() {
                        Some(types) => types.intersection(&names).copied().collect(),
                        None => names,
                    });
                }
            }
            "properties" => {
                for (name, property) in value.as_object().into_iter().flatten() {
                    self.properties.entry(name).or_default().push(property);
                }
            }
            "required" => self.required.extend(
                value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            ),
            "additionalProperties" => self.additional.push(value),
            "items" => self.items.push(value),
            "enum" => {
                let listed: BTreeSet<String> = value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(Value::to_string)
                    .collect();
                self.values = Some(match self.values.take() {
                    Some(values) => values.intersection(&listed).cloned().collect(),
                    None => listed,
                });
            }
            "contentSchema" => self.content.push(value),
            key if ANNOTATIONS.contains(&key) => {}
            key => {
                let given = self.others.entry(key).or_default();
                if !given.contains(&value) {
                    given.push(value);
                }
            }
        }
        Ok(())
    }

    /// The one value that the schemas give the keyword `key`, `None` where none gives it, or
    /// that two give it different values, which the diff can't read.
    pub fn other(&self, key: &str) -> Result<Option<&'a Value>, String> {
        match self.others.get(key).map(Vec::as_slice) {
            None | Some([]) => Ok(None),
            Some([value]) => Ok(Some(value)),
            Some(_) => Err(format!("two schemas of one value give it a `{key}` each")),
        }
    }
}

/// The schema of `document` that `reference` names: `#/components/schemas/<name>`.
fn referred<'a>(document: &'a Value, reference: &Value) -> Result<&'a Value, String> {
    reference
        .as_str()
        .and_then(|target| target.strip_prefix("#/components/schemas/"))
        .and_then(|name| document["components"]["schemas"].get(name))
        .ok_or(format!(
            "the reference `{reference}` names no schema of the document"
        ))
}

/// The name of a `oneOf`'s member: the schema that it refers to, or its place in the list.
fn alternative(index: usize, member: &Value) -> String {
    member["$ref"]
        .as_str()
        .and_then(|target| target.rsplit('/').next())
        .map_or_else(|| format!("{}", index + 1), str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn api_18_a_shape_reads_a_schema_through_its_references_and_its_all_of() {
        let document = json!({"components": {"schemas": {
            "Problem": {
                "type": "object",
                "description": "A problem.",
                "required": ["code"],
                "properties": {"code": {"type": "string"}, "status": {"type": "integer"}}
            }
        }}});
        let schema = json!({"allOf": [
            {"$ref": "#/components/schemas/Problem"},
            {"type": "object", "properties": {"code": {"type": "string", "enum": ["a.b", "a.c"]}}}
        ]});

        let shape = Shape::of(&document, &[&schema]).unwrap();
        let code = Shape::of(&document, &shape.properties["code"]).unwrap();

        assert_eq!(shape.types, Some(BTreeSet::from(["object"])));
        assert_eq!(shape.required, BTreeSet::from(["code"]));
        assert_eq!(
            shape.properties.keys().copied().collect::<Vec<_>>(),
            ["code", "status"]
        );
        assert!(shape.others.is_empty());
        assert_eq!(
            code.values,
            Some(BTreeSet::from(["\"a.b\"".to_owned(), "\"a.c\"".to_owned()]))
        );
    }

    #[test]
    fn api_18_a_shape_reads_null_in_a_type_and_in_a_one_of() {
        let document =
            json!({"components": {"schemas": {"Name": {"type": "string", "maxLength": 9}}}});
        let listed = json!({"type": ["integer", "null"]});
        let either = json!({"oneOf": [{"type": "null"}, {"$ref": "#/components/schemas/Name"}]});
        let plain = json!({"type": "string"});

        let listed = Shape::of(&document, &[&listed]).unwrap();
        let either = Shape::of(&document, &[&either]).unwrap();
        let plain = Shape::of(&document, &[&plain]).unwrap();

        assert!(listed.nullable && either.nullable && !plain.nullable);
        assert_eq!(listed.types, Some(BTreeSet::from(["integer"])));
        assert_eq!(either.types, Some(BTreeSet::from(["string"])));
        assert!(either.alternatives.is_empty());
        assert_eq!(either.other("maxLength"), Ok(Some(&json!(9))));
    }

    #[test]
    fn api_18_a_shape_names_the_alternatives_of_a_one_of() {
        let document = json!({"components": {"schemas": {"A": {}, "B": {}}}});
        let schema = json!({"oneOf": [
            {"$ref": "#/components/schemas/A"},
            {"type": "string"},
            {"$ref": "#/components/schemas/B"}
        ]});

        let shape = Shape::of(&document, &[&schema]).unwrap();

        let names: Vec<&str> = shape
            .alternatives
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(names, ["A", "2", "B"]);
    }

    #[test]
    fn api_18_a_shape_says_what_it_can_t_read() {
        let document = json!({"components": {"schemas": {"A": {"oneOf": [{"type": "string"}, {"type": "integer"}]}}}});
        let unread = |schema: Value| Shape::of(&document, &[&schema]).err();

        assert_eq!(
            unread(json!("string")).as_deref(),
            Some("the schema `\"string\"` is no object")
        );
        assert_eq!(
            unread(json!({"$ref": "#/components/schemas/Gone"})).as_deref(),
            Some("the reference `\"#/components/schemas/Gone\"` names no schema of the document")
        );
        assert_eq!(
            unread(json!({"$ref": "#/components/responses/A"})).as_deref(),
            Some("the reference `\"#/components/responses/A\"` names no schema of the document")
        );
        assert_eq!(
            unread(json!({"type": 3})).as_deref(),
            Some("the type `3` is no name or names")
        );
        assert_eq!(
            unread(json!({"allOf": [
                {"$ref": "#/components/schemas/A"},
                {"oneOf": [{"type": "boolean"}, {"type": "array"}]}
            ]}))
            .as_deref(),
            Some("two `oneOf` hold of one value")
        );
        let twice = json!({"allOf": [{"format": "uuid"}, {"format": "date"}, {"format": "uuid"}]});
        assert_eq!(
            Shape::of(&document, &[&twice]).unwrap().other("format"),
            Err("two schemas of one value give it a `format` each".to_owned())
        );
    }

    #[test]
    fn api_18_a_shape_reads_the_schema_true_as_one_that_says_nothing_and_can_t_read_false() {
        let document = json!({"components": {"schemas": {}}});
        let string = json!({"type": "string"});

        let beside = Shape::of(&document, &[&json!(true), &string]).unwrap();
        let alone = Shape::of(&document, &[&json!(true)]).unwrap();
        let refused = Shape::of(&document, &[&json!(false)]).err();

        assert_eq!(beside.types, Some(BTreeSet::from(["string"])));
        assert_eq!(alone.types, None);
        assert!(alone.properties.is_empty() && alone.others.is_empty() && !alone.nullable);
        assert_eq!(refused.as_deref(), Some("the schema `false` is no object"));
    }

    #[test]
    fn api_18_a_shape_that_refers_to_itself_is_read_once() {
        let document = json!({"components": {"schemas": {
            "Loop": {"allOf": [{"$ref": "#/components/schemas/Loop"}], "type": "object"}
        }}});
        let schema = json!({"$ref": "#/components/schemas/Loop"});

        let shape = Shape::of(&document, &[&schema]).unwrap();

        assert_eq!(shape.types, Some(BTreeSet::from(["object"])));
    }
}
