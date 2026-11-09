//! What the OpenAPI document says of a file's download beyond what every download's says: the
//! `Cache-Control` of a public download, and the query of a signed URL that a private file's
//! download takes (PLATFORM-080, FILES-023, API-17).

use utoipa::openapi::{
    ObjectBuilder, RefOr, Type,
    path::{ParameterBuilder, ParameterIn, Paths},
};

use super::{header, operations};
use crate::signed_urls::{EXPIRES, SESSION, SIGNATURE};

/// Documents on each response of a public download among `paths`, a 200, 206 or 304 that
/// [`super::download`] documented, its `Cache-Control`, since shared caches may keep them
/// (PLATFORM-081, FILES-023).
pub(in crate::http) fn public(paths: &mut Paths) {
    for item in paths.paths.values_mut() {
        for (_, operation) in operations(item) {
            for status in ["200", "206", "304"] {
                if let Some(RefOr::T(response)) = operation.responses.responses.get_mut(status) {
                    response.headers.insert(
                        "cache-control".to_owned(),
                        header("How long a shared cache may keep the download.").into(),
                    );
                }
            }
        }
    }
}

/// Documents on each operation of `paths` the query parameters of its signed URL, which a
/// download route takes in place of a credential (PLATFORM-080).
pub(in crate::http) fn signed(paths: &mut Paths) {
    let parameters = [
        (
            EXPIRES,
            ObjectBuilder::new().schema_type(Type::Integer).build(),
            "The Unix time, in seconds, at which the signed URL stops working.",
        ),
        (
            SESSION,
            ObjectBuilder::new()
                .schema_type(Type::String)
                .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(
                    utoipa::openapi::KnownFormat::Uuid,
                )))
                .build(),
            "The session the signed URL acts for.",
        ),
        (
            SIGNATURE,
            ObjectBuilder::new()
                .schema_type(Type::String)
                .min_length(Some(43))
                .max_length(Some(43))
                .build(),
            "The signed URL's signature, as the API gave it. With `expires` and `session`, it authenticates the request in place of a credential.",
        ),
    ];
    for item in paths.paths.values_mut() {
        for (_, operation) in operations(item) {
            let list = operation.parameters.get_or_insert_default();
            for (name, schema, description) in &parameters {
                list.push(
                    ParameterBuilder::new()
                        .name(*name)
                        .parameter_in(ParameterIn::Query)
                        .required(utoipa::openapi::Required::False)
                        .schema(Some(schema.clone()))
                        .description(Some(*description))
                        .build()
                        .into(),
                );
            }
        }
    }
}
