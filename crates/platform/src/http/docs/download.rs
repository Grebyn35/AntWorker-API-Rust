//! What the OpenAPI document says of a download beyond its handler's 200: the 206 and 304 that
//! PLATFORM-081 serves, with the headers that bring them and that they carry (API-17).

use utoipa::openapi::{
    ObjectBuilder, RefOr, Response, Type,
    path::{ParameterBuilder, ParameterIn, Paths},
};

use super::{header, operations};

/// Documents on each operation of `paths`, a download's, what PLATFORM-081 serves besides the 200
/// that its handler documents: a 206 of a range of its bytes, with the 200's content and
/// `Content-Range`, and a 304, and on all three its `ETag`, and `Accept-Ranges` on the two that
/// serve bytes, with the request's `Range`, `If-Range` and `If-None-Match` that bring them.
pub(in crate::http) fn download(paths: &mut Paths) {
    for item in paths.paths.values_mut() {
        for (_, operation) in operations(item) {
            let responses = &mut operation.responses.responses;
            let Some(RefOr::T(whole)) = responses.get("200").cloned() else {
                continue;
            };
            let mut range = whole.clone();
            range.description = "A range of the bytes, as `Range` asked for it.".to_owned();
            range.headers.insert(
                "content-range".to_owned(),
                header("The range served, and the size of all the bytes: `bytes <first>-<last>/<size>`.")
                    .into(),
            );
            let mut not_modified =
                Response::new("Nothing: the client holds the bytes its `If-None-Match` names.");
            for response in [&mut range, &mut not_modified] {
                response.headers.insert(
                    "etag".to_owned(),
                    header("The entity tag that names the bytes, which never change.").into(),
                );
            }
            let mut whole = whole;
            for response in [&mut whole, &mut range] {
                response.headers.insert(
                    "etag".to_owned(),
                    header("The entity tag that names the bytes, which never change.").into(),
                );
                response.headers.insert(
                    "accept-ranges".to_owned(),
                    header("`bytes`: a request may ask for one range of them.").into(),
                );
            }
            responses.insert("200".to_owned(), RefOr::T(whole));
            responses.insert("206".to_owned(), RefOr::T(range));
            responses.insert("304".to_owned(), RefOr::T(not_modified));
            let parameters = operation.parameters.get_or_insert_default();
            for (name, description) in [
                (
                    "Range",
                    "One range of the bytes, `bytes=first-last`, `bytes=first-` or `bytes=-suffix`, which a 206 serves (PLATFORM-081).",
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
