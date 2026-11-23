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
    use utoipa::PartialSchema;
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
}
