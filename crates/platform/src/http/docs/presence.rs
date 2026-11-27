//! Which fields a response always holds (API-05). A field that a response may leave out is
//! documented as one that is never `null`, so a field that may be `null` is one that every
//! response holds, as its value or as `null`. The document requires each such field, which
//! `utoipa` doesn't of a field whose type is an `Option`.

use std::collections::BTreeSet;

use serde_json::Value;
use utoipa::openapi::{OpenApi, RefOr, Schema, schema::ArrayItems};

/// The start of a reference to a component that is a schema.
const SCHEMAS: &str = "#/components/schemas/";

/// Requires every property that `nullable` finds may be `null` of the schemas in the responses
/// of `document` themselves, and of each component but those that `asked` names, which a
/// request gives: a request may leave out a field that may be `null`.
pub(in crate::http) fn require_nullable(document: &mut OpenApi) {
    let asked = asked(document);
    for item in document.paths.paths.values_mut() {
        for (_, operation) in super::operations(item) {
            for response in operation.responses.responses.values_mut() {
                let RefOr::T(response) = response else {
                    continue;
                };
                for content in response.content.values_mut() {
                    if let RefOr::T(content) = content
                        && let Some(schema) = &mut content.schema
                    {
                        require(schema);
                    }
                }
            }
        }
    }
    let Some(components) = &mut document.components else {
        return;
    };
    for (name, schema) in &mut components.schemas {
        if !asked.contains(name) {
            require(schema);
        }
    }
}

/// The names of the components that a request's body or its parameters refer to, in `document`,
/// or that such a component refers to, and none when `serde_json` can't write `document` as a
/// JSON value.
fn asked(document: &OpenApi) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let Ok(document) = serde_json::to_value(document) else {
        return found;
    };
    let mut pending = Vec::new();
    for item in document["paths"].as_object().into_iter().flatten() {
        references(&item.1["parameters"], &mut pending);
        for operation in item.1.as_object().into_iter().flatten() {
            references(&operation.1["requestBody"], &mut pending);
            references(&operation.1["parameters"], &mut pending);
        }
    }
    while let Some(name) = pending.pop() {
        if found.insert(name.clone()) {
            references(&document["components"]["schemas"][&name], &mut pending);
        }
    }
    found
}

/// Adds to `names` the name of each component that a `$ref` anywhere in `value` names.
fn references(value: &Value, names: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if let Some(name) = text.strip_prefix(SCHEMAS) {
                names.push(name.to_owned());
            }
        }
        Value::Array(items) => items.iter().for_each(|item| references(item, names)),
        Value::Object(members) => members
            .values()
            .for_each(|member| references(member, names)),
        _ => {}
    }
}

/// Requires every property of `schema` that `nullable` finds may be `null`, and those of the
/// schemas it holds.
fn require(schema: &mut RefOr<Schema>) {
    let RefOr::T(schema) = schema else {
        return;
    };
    match schema {
        Schema::Object(object) => {
            for (name, property) in &mut object.properties {
                if nullable(property) && !object.required.contains(name) {
                    object.required.push(name.clone());
                }
                require(property);
            }
        }
        Schema::Array(array) => {
            if let ArrayItems::RefOrSchema(items) = &mut array.items {
                require(items);
            }
        }
        Schema::OneOf(one_of) => one_of.items.iter_mut().for_each(require),
        Schema::AllOf(all_of) => all_of.items.iter_mut().for_each(require),
        Schema::AnyOf(any_of) => any_of.items.iter_mut().for_each(require),
        _ => {}
    }
}

/// Whether `schema` takes `null`: its `type` holds it, or one of its `oneOf` is it. A schema that
/// `serde_json` can't write as a JSON value reads as one that doesn't.
fn nullable(schema: &RefOr<Schema>) -> bool {
    let Ok(schema) = serde_json::to_value(schema) else {
        return false;
    };
    let null = |schema: &Value| match &schema["type"] {
        Value::String(kind) => kind == "null",
        Value::Array(kinds) => kinds.iter().any(|kind| kind == "null"),
        _ => false,
    };
    null(&schema) || schema["oneOf"].as_array().into_iter().flatten().any(null)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use utoipa::openapi::{
        AllOfBuilder, ArrayBuilder, ComponentsBuilder, ContentBuilder, ObjectBuilder, OneOfBuilder,
        OpenApi, OpenApiBuilder, PathItem, PathsBuilder, Ref, RefOr, ResponseBuilder, Schema,
        path::{HttpMethod, OperationBuilder},
        request_body::RequestBodyBuilder,
        schema::{AnyOfBuilder, SchemaType, Type},
    };

    /// A schema of `kind` or `null`.
    fn or_null(kind: Type) -> ObjectBuilder {
        ObjectBuilder::new().schema_type(SchemaType::from_iter([kind, Type::Null]))
    }

    /// An object whose one property, `note`, may be `null`.
    fn noted() -> ObjectBuilder {
        ObjectBuilder::new().property("note", or_null(Type::String))
    }

    /// The content of a media type, whose schema is `schema`.
    fn json_content(schema: impl Into<RefOr<Schema>>) -> utoipa::openapi::Content {
        ContentBuilder::new().schema(Some(schema)).build()
    }

    /// The document of `POST /v1/things`, which takes `body`, if any, and answers `answer`, with
    /// `components`, once `require_nullable` has read it, as JSON.
    fn required_in(
        body: Option<RefOr<Schema>>,
        answer: impl Into<RefOr<Schema>>,
        components: ComponentsBuilder,
    ) -> Value {
        let mut operation = OperationBuilder::new().response(
            "200",
            ResponseBuilder::new()
                .description("Done.")
                .content("application/json", json_content(answer)),
        );
        if let Some(body) = body {
            operation = operation.request_body(Some(
                RequestBodyBuilder::new()
                    .content("application/json", json_content(body))
                    .build(),
            ));
        }
        let paths =
            PathsBuilder::new().path("/v1/things", PathItem::new(HttpMethod::Post, operation));
        let mut document: OpenApi = OpenApiBuilder::new()
            .paths(paths)
            .components(Some(components.build()))
            .build();

        super::require_nullable(&mut document);

        serde_json::to_value(document).unwrap()
    }

    /// The names of the properties that `schema` requires, in alphabetical order.
    fn names(schema: &Value) -> Vec<&str> {
        let mut names: Vec<&str> = schema["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn api_05_a_response_s_property_that_may_be_null_is_required_at_any_depth() {
        let answer = ObjectBuilder::new()
            .property("direct", or_null(Type::Integer))
            .property(
                "linked",
                OneOfBuilder::new()
                    .item(Ref::from_schema_name("Thing"))
                    .item(ObjectBuilder::new().schema_type(Type::Null)),
            )
            .property(
                "either",
                ObjectBuilder::new()
                    .schema_type(SchemaType::from_iter([Type::String, Type::Integer])),
            )
            .property("list", ArrayBuilder::new().items(noted()))
            .property("choice", OneOfBuilder::new().item(noted()))
            .property(
                "both",
                AllOfBuilder::new()
                    .item(noted())
                    .item(ObjectBuilder::new().description(Some("Both."))),
            )
            .property("any", AnyOfBuilder::new().item(noted()));

        let document = required_in(None, answer, ComponentsBuilder::new());

        let answer = &document["paths"]["/v1/things"]["post"]["responses"]["200"]["content"]["application/json"]
            ["schema"];
        assert_eq!(names(answer), ["direct", "linked"]);
        for nested in [
            "/properties/list/items",
            "/properties/choice/oneOf/0",
            "/properties/both/allOf/0",
            "/properties/any/anyOf/0",
        ] {
            assert_eq!(names(answer.pointer(nested).unwrap()), ["note"], "{nested}");
        }
    }

    #[test]
    fn api_05_a_component_that_a_request_refers_to_keeps_a_property_that_may_be_null_unrequired() {
        let request = ObjectBuilder::new().property(
            "detail",
            OneOfBuilder::new()
                .item(Ref::from_schema_name("Detail"))
                .item(ObjectBuilder::new().schema_type(Type::Null)),
        );
        let components = ComponentsBuilder::new()
            .schema("Request", request)
            .schema("Detail", noted())
            .schema("Answer", noted());

        let document = required_in(
            Some(Ref::from_schema_name("Request").into()),
            Ref::from_schema_name("Answer"),
            components,
        );

        let schemas = &document["components"]["schemas"];
        assert_eq!(names(&schemas["Answer"]), ["note"]);
        assert!(names(&schemas["Request"]).is_empty());
        assert!(names(&schemas["Detail"]).is_empty());
    }
}
