//! What the OpenAPI document says of a download beyond its handler's 200: the 206 and 304 that
//! PLATFORM-081 serves, with the headers that bring them and that they carry, and the query of a
//! signed URL that a private file's download takes (PLATFORM-080, API-17).

use utoipa::openapi::{
    ObjectBuilder, RefOr, Response, Type,
    path::{ParameterBuilder, ParameterIn, Paths},
};

use super::{header, operations};
use crate::signed_urls::{EXPIRES, SESSION, SIGNATURE};

/// Documents on each operation of `paths`, a download's, what PLATFORM-081 serves besides the 200
/// that its handler documents: a 206 of a range of its bytes, with the 200's content and
/// `Content-Range`, and a 304, and on all three its `ETag`, and `Accept-Ranges` on the two that
/// serve bytes, with the request's `Range`, `If-Range` and `If-None-Match` that bring them. A
/// public download's responses also document `Cache-Control`, since shared caches may keep
/// them.
pub(in crate::http) fn download(paths: &mut Paths, public: bool) {
    for item in paths.paths.values_mut() {
        for (_, operation) in operations(item) {
            let responses = &mut operation.responses.responses;
            let Some(RefOr::T(whole)) = responses.get("200").cloned() else {
                continue;
            };
            let mut range = whole.clone();
            range.description = "A range of the file's bytes, as `Range` asked for it.".to_owned();
            range.headers.insert(
                "content-range".to_owned(),
                header("The range served, and the file's size: `bytes <first>-<last>/<size>`.")
                    .into(),
            );
            let mut not_modified =
                Response::new("Nothing: the client holds the bytes its `If-None-Match` names.");
            for response in [&mut range, &mut not_modified] {
                response.headers.insert(
                    "etag".to_owned(),
                    header("The entity tag that names the file's bytes, which never change.")
                        .into(),
                );
            }
            let mut whole = whole;
            for response in [&mut whole, &mut range] {
                response.headers.insert(
                    "etag".to_owned(),
                    header("The entity tag that names the file's bytes, which never change.")
                        .into(),
                );
                response.headers.insert(
                    "accept-ranges".to_owned(),
                    header("`bytes`: a request may ask for one range of them.").into(),
                );
            }
            if public {
                for response in [&mut whole, &mut range, &mut not_modified] {
                    response.headers.insert(
                        "cache-control".to_owned(),
                        header("How long a shared cache may keep the download.").into(),
                    );
                }
            }
            responses.insert("200".to_owned(), RefOr::T(whole));
            responses.insert("206".to_owned(), RefOr::T(range));
            responses.insert("304".to_owned(), RefOr::T(not_modified));
            let parameters = operation.parameters.get_or_insert_default();
            for (name, description) in [
                (
                    "Range",
                    "One range of the file's bytes, `bytes=first-last`, `bytes=first-` or `bytes=-suffix`, which a 206 serves (PLATFORM-081).",
                ),
                (
                    "If-Range",
                    "The entity tag of the bytes a client holds part of: the range is served only while it names them.",
                ),
                (
                    "If-None-Match",
                    "The entity tags of bytes the client holds, or `*`: a 304 answers while one names them.",
                ),
            ] {
                parameters.push(
                    ParameterBuilder::new()
                        .name(name)
                        .parameter_in(ParameterIn::Header)
                        .required(utoipa::openapi::Required::False)
                        .schema(Some(ObjectBuilder::new().schema_type(Type::String).build()))
                        .description(Some(description))
                        .build()
                        .into(),
                );
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
