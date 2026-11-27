//! The schema of a header parameter (API-17). A header has a value or is absent, and is never
//! `null`, which utoipa's schema of a parameter declared as an `Option` offers.

use utoipa::openapi::{
    OneOf, RefOr, Schema, Type,
    path::{Operation, ParameterIn},
    schema::{AnyOf, SchemaType},
};

/// Takes `null` out of the schema of each of `operation`'s header parameters where it stands
/// beside another alternative: out of a `type` that holds another type too, and out of the
/// members of a `oneOf` or an `anyOf`, whose one member left then stands in its place where it
/// holds nothing else.
pub(super) fn without_null(operation: &mut Operation) {
    for parameter in operation.parameters.iter_mut().flatten() {
        if let RefOr::T(parameter) = parameter
            && matches!(parameter.parameter_in, ParameterIn::Header)
            && let Some(schema) = &mut parameter.schema
        {
            take_null(schema);
        }
    }
}

fn take_null(schema: &mut RefOr<Schema>) {
    let alone = match schema {
        RefOr::T(Schema::Object(object)) => {
            other_types(&mut object.schema_type);
            None
        }
        RefOr::T(Schema::Array(array)) => {
            other_types(&mut array.schema_type);
            None
        }
        RefOr::T(Schema::OneOf(one_of)) => {
            let bare = OneOf {
                items: Vec::new(),
                ..one_of.clone()
            } == OneOf::new();
            other_members(&mut one_of.items, bare)
        }
        RefOr::T(Schema::AnyOf(any_of)) => {
            let bare = AnyOf {
                items: Vec::new(),
                ..any_of.clone()
            } == AnyOf::new();
            other_members(&mut any_of.items, bare)
        }
        _ => None,
    };
    if let Some(member) = alone {
        *schema = member;
    }
}

/// Takes `null` out of `types` where they hold another type too.
fn other_types(types: &mut SchemaType) {
    let SchemaType::Array(kinds) = types else {
        return;
    };
    let others: Vec<Type> = kinds
        .iter()
        .filter(|kind| **kind != Type::Null)
        .cloned()
        .collect();
    match others.as_slice() {
        [] => {}
        [kind] => *types = SchemaType::Type(kind.clone()),
        _ => *kinds = others,
    }
}

/// Takes each member `{"type": "null"}` out of `members` where another member stays, and gives the
/// one member left once one is taken, where the composite holds nothing else, `bare`.
fn other_members(members: &mut Vec<RefOr<Schema>>, bare: bool) -> Option<RefOr<Schema>> {
    let null = |member: &RefOr<Schema>| {
        matches!(member, RefOr::T(Schema::Object(object))
            if object.schema_type == SchemaType::Type(Type::Null))
    };
    if members.iter().all(null) || !members.iter().any(null) {
        return None;
    }
    members.retain(|member| !null(member));
    (bare && members.len() == 1).then(|| members.remove(0))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use utoipa::{
        PartialSchema,
        openapi::{
            ArrayBuilder, ObjectBuilder, OneOfBuilder, RefOr, Schema,
            path::{OperationBuilder, ParameterBuilder, ParameterIn},
            schema::{AnyOfBuilder, SchemaType, Type},
        },
    };
    use utoipa_axum::routes;

    use crate::{http::Routes, idempotency::IdempotencyKey, telemetry::Service};

    #[utoipa::path(
        post,
        path = "/v1/things",
        params(
            ("If-Match" = Option<String>, Header, description = "A version."),
            ("Idempotency-Key" = inline(Option<IdempotencyKey>), Header, description = "A key."),
        ),
        responses((status = 204, description = "Done.")),
    )]
    async fn make() {}

    #[test]
    fn api_17_a_header_parameter_declared_as_an_option_offers_no_null() {
        let routes = Routes::<()>::new().public(routes!(make), &[]);

        let document = routes.openapi(Service {
            name: "test",
            version: "0.0.0",
        });

        let document = serde_json::to_value(&document).unwrap();
        let schemas: Vec<(&Value, &Value)> = document["paths"]["/v1/things"]["post"]["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|parameter| (&parameter["name"], &parameter["schema"]))
            .collect();
        let key = serde_json::to_value(IdempotencyKey::schema()).unwrap();
        assert_eq!(
            schemas,
            [
                (&json!("If-Match"), &json!({ "type": "string" })),
                (&json!("Idempotency-Key"), &key),
            ]
        );
    }

    /// What `without_null` leaves of `schema` as a header parameter's, in the document's JSON.
    fn without_null_in(schema: impl Into<RefOr<Schema>>) -> Value {
        let header = ParameterBuilder::new()
            .name("X-Thing")
            .parameter_in(ParameterIn::Header)
            .schema(Some(schema));
        let mut operation = OperationBuilder::new().parameter(header).build();

        super::without_null(&mut operation);

        serde_json::to_value(operation.parameters).unwrap()[0]["schema"].clone()
    }

    fn of_type(kind: Type) -> ObjectBuilder {
        ObjectBuilder::new().schema_type(kind)
    }

    #[test]
    fn api_17_null_beside_the_type_of_a_header_s_array_is_taken_out() {
        let list = ArrayBuilder::new()
            .schema_type(SchemaType::from_iter([Type::Array, Type::Null]))
            .items(of_type(Type::String));

        assert_eq!(
            without_null_in(list),
            json!({ "type": "array", "items": { "type": "string" } })
        );
    }

    #[test]
    fn api_17_null_beside_a_member_of_a_header_s_any_of_is_taken_out() {
        let any_of = AnyOfBuilder::new()
            .item(of_type(Type::String))
            .item(of_type(Type::Null));

        assert_eq!(without_null_in(any_of), json!({ "type": "string" }));
    }

    #[test]
    fn api_17_a_header_s_one_of_or_any_of_keeps_what_it_holds_but_null() {
        let described = OneOfBuilder::new()
            .item(of_type(Type::String))
            .item(of_type(Type::Null))
            .description(Some("A version."));
        let two = AnyOfBuilder::new()
            .item(of_type(Type::String))
            .item(of_type(Type::Integer))
            .item(of_type(Type::Null));

        assert_eq!(
            without_null_in(described),
            json!({ "oneOf": [{ "type": "string" }], "description": "A version." })
        );
        assert_eq!(
            without_null_in(two),
            json!({ "anyOf": [{ "type": "string" }, { "type": "integer" }] })
        );
    }

    #[test]
    fn api_17_null_beside_no_other_alternative_is_left_in_a_header_s_schema() {
        let null_alone = ObjectBuilder::new().schema_type(SchemaType::from_iter([Type::Null]));
        let null_member = OneOfBuilder::new().item(of_type(Type::Null));

        assert_eq!(without_null_in(null_alone), json!({ "type": ["null"] }));
        assert_eq!(
            without_null_in(null_member),
            json!({ "oneOf": [{ "type": "null" }] })
        );
    }
}
