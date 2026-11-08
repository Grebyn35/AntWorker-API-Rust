//! What the OpenAPI document says beyond the handlers' own annotations: each operation's access
//! level, and every problem the pipeline can answer, with its code and meaning (API-17,
//! ERR-13).

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::http::Method;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{
        AllOfBuilder, Components, Content, ObjectBuilder, OneOfBuilder, OpenApi, Ref, RefOr,
        Response, Schema, Type,
        extensions::Extensions,
        header::HeaderBuilder,
        path::{Operation, ParameterBuilder, ParameterIn, PathItem, Paths},
    },
};

use super::Access;
use crate::{
    errors::{ErrorCode, ProblemBody, RequestError},
    events::catalog::EVENT_EXTENSION,
    signed_urls::{EXPIRES, SESSION, SIGNATURE},
};

/// The component of the event catalog, which holds every frame type (REALTIME-008).
const FRAME: &str = "Frame";

/// The media type of the stream of server-sent events.
const EVENT_STREAM: &str = "text/event-stream";

/// The extension of the stream's media type that names the schema of each of its frames, as
/// OpenAPI 3.2's `itemSchema` does, which OpenAPI 3.1 lacks (REALTIME-008).
const ITEM_SCHEMA: &str = "x-itemSchema";

/// The query parameters each method of a path accepts (API-09).
pub(super) type QueryNames = HashMap<Method, HashSet<String>>;

/// The media type of every error response (API-07).
const PROBLEM_MEDIA_TYPE: &str = "application/problem+json";

/// The header of an idempotency key (API-12).
pub(super) const IDEMPOTENCY_KEY: &str = "idempotency-key";

/// The header of an update's precondition (API-13).
const IF_MATCH: &str = "if-match";

/// Where the document lists a code that `platform` answers.
enum Place {
    /// On every operation: the route's own pipeline answers it.
    Every,
    /// On every operation whose method isn't safe: the cross-site check answers it (PLATFORM-059).
    Unsafe,
    /// On every operation that authenticates its caller (AUTH-009).
    Authenticated,
    /// On every operation that takes a body (PLATFORM-049, PLATFORM-051).
    Body,
    /// On every operation of a workspace route, which authorization guards (ACCESS-003).
    Workspace,
    /// On every operation that takes an `Idempotency-Key` (PLATFORM-036, PLATFORM-037).
    Keyed,
    /// On every operation that takes an `If-Match` (PLATFORM-071).
    Conditional,
    /// On every operation that takes a signed URL (PLATFORM-080).
    Signed,
    /// Among the components' responses: the router answers it when no operation matches.
    Fallback,
}

fn place(error: &RequestError) -> Place {
    match error {
        RequestError::NotFound | RequestError::MethodNotAllowed => Place::Fallback,
        RequestError::OriginNotAllowed => Place::Unsafe,
        RequestError::PermissionDenied => Place::Workspace,
        RequestError::InvalidIdempotencyKey
        | RequestError::IdempotencyKeyReused
        | RequestError::IdempotencyKeyInFlight => Place::Keyed,
        RequestError::PreconditionFailed => Place::Conditional,
        RequestError::Unauthenticated | RequestError::AccessTokenExpired => Place::Authenticated,
        RequestError::SignedUrlExpired => Place::Signed,
        RequestError::InvalidBody(_) | RequestError::InvalidFields(_) => Place::Body,
        RequestError::InvalidQuery(_)
        | RequestError::PayloadTooLarge
        | RequestError::RateLimited
        | RequestError::TimedOut
        | RequestError::Overloaded
        | RequestError::Unavailable
        | RequestError::Panicked => Place::Every,
    }
}

/// Whether the problems of `place` apply to `operation`, served with `method` at `access`.
fn applies(place: &Place, method: &Method, access: Access, operation: &Operation) -> bool {
    match place {
        Place::Every => true,
        Place::Unsafe => !method.is_safe(),
        Place::Authenticated => !matches!(access, Access::Public),
        Place::Body => operation.request_body.is_some(),
        Place::Workspace => matches!(access, Access::Workspace(_)),
        Place::Keyed => takes_header(operation, IDEMPOTENCY_KEY),
        Place::Conditional => takes_header(operation, IF_MATCH),
        Place::Signed => query_names(operation).contains(SIGNATURE),
        Place::Fallback => false,
    }
}

/// Whether `operation` documents the header `name`, such as `Idempotency-Key` (API-12).
fn takes_header(operation: &Operation, name: &str) -> bool {
    operation
        .parameters
        .iter()
        .flatten()
        .any(|parameter| match parameter {
            RefOr::T(parameter) => {
                matches!(parameter.parameter_in, ParameterIn::Header)
                    && parameter.name.eq_ignore_ascii_case(name)
            }
            RefOr::Ref(_) => false,
        })
}

/// Whether the pipeline can answer `error` on `operation`. A workspace route answers
/// `request.not_found` itself, to a caller who isn't a member (ACCESS-003), besides the router's
/// answer to a path no route has.
fn answers(error: &RequestError, method: &Method, access: Access, operation: &Operation) -> bool {
    applies(&place(error), method, access, operation)
        || (matches!(error, RequestError::NotFound) && matches!(access, Access::Workspace(_)))
}

/// Documents each operation's access level as the extension `x-access`, a workspace route's
/// permission as `x-permission`, which TEST-08's generated test reads, the problems the
/// pipeline can answer on it, and the rate-limit headers of every response the limiter passes
/// back through (PLATFORM-056). A route that documents its own response for one of those
/// statuses keeps it, and lists the pipeline's codes in it itself (ERR-13). Returns the query
/// parameters that each method accepts, `HEAD` taking those of the `GET` that serves it.
pub(super) fn document(paths: &mut Paths, access: Access, errors: &[&dyn ErrorCode]) -> QueryNames {
    let mut queries = QueryNames::new();
    for item in paths.paths.values_mut() {
        for (method, operation) in operations(item) {
            let mut extensions = vec![("x-access", serde_json::Value::from(access.name()))];
            if let Access::Workspace(permission) = access {
                extensions.push(("x-permission", serde_json::Value::from(permission)));
            }
            operation.extensions = Some(extensions.into_iter().collect());
            for (status, response) in problems(&method, access, operation, errors) {
                operation
                    .responses
                    .responses
                    .entry(status)
                    .or_insert(RefOr::T(response));
            }
            rate_limit_headers(operation);
            let names = query_names(operation);
            if method == Method::GET {
                queries.insert(Method::HEAD, names.clone());
            }
            queries.insert(method, names);
        }
    }
    queries
}

/// Documents on each operation of `paths`, a download's, what PLATFORM-081 serves besides the 200
/// that its handler documents: a 206 of a range of its bytes, with the 200's content and
/// `Content-Range`, and a 304, and on all three its `ETag`, and `Accept-Ranges` on the two that
/// serve bytes. A public download's responses also document `Cache-Control`, since shared caches
/// may keep them.
pub(super) fn download(paths: &mut Paths, public: bool) {
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
        }
    }
}

/// Documents on each operation of `paths` the query parameters of its signed URL, which a
/// download route takes in place of a credential (PLATFORM-080).
pub(super) fn signed(paths: &mut Paths) {
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

/// Adds the rate-limit headers to each of `operation`'s responses but the 5xx: a request that
/// load shedding refuses never reaches the limiter, and a panic never passes back through it
/// (PLATFORM-056).
fn rate_limit_headers(operation: &mut Operation) {
    for (status, response) in &mut operation.responses.responses {
        if status.starts_with('5') {
            continue;
        }
        if let RefOr::T(response) = response {
            response.headers.insert(
                "ratelimit".to_owned(),
                header(
                    "The requests left in the bucket that decided the response, and the seconds until its window ends.",
                )
                .into(),
            );
            response.headers.insert(
                "ratelimit-policy".to_owned(),
                header("That bucket's quota, and its window in seconds.").into(),
            );
        }
    }
}

/// The schema of every problem, as `Problem` in the document's components, with the schemas it
/// refers to, such as `FieldError`.
pub(super) fn problem_schemas() -> Vec<(String, RefOr<Schema>)> {
    let mut schemas = vec![(ProblemBody::name().into_owned(), ProblemBody::schema())];
    ProblemBody::schemas(&mut schemas);
    schemas
}

/// Adds to `document`'s components the problems the router answers when no operation matches a
/// request: `NotFound` for a path that no route has, and `MethodNotAllowed`, with `Allow`, for a
/// method its path doesn't serve.
pub(super) fn fallbacks(document: &mut OpenApi) {
    let components = document.components.get_or_insert_with(Components::new);
    for (name, schema) in problem_schemas() {
        components.schemas.entry(name).or_insert(schema);
    }
    for error in RequestError::every() {
        if !matches!(place(&error), Place::Fallback) {
            continue;
        }
        let mut response = problem_response(&[&error]);
        let name = match error {
            RequestError::MethodNotAllowed => {
                response.headers.insert(
                    "allow".to_owned(),
                    header("The methods the path serves.").into(),
                );
                "MethodNotAllowed"
            }
            _ => "NotFound",
        };
        components
            .responses
            .insert(name.to_owned(), RefOr::T(response));
    }
}

/// Builds the event catalog `Frame`, a `oneOf` of every frame type's schema that `document`'s
/// components mark, in the order of their names, and names it in each `text/event-stream` media
/// type of `document`'s responses (REALTIME-008).
pub(super) fn catalog(document: &mut OpenApi) {
    let components = document.components.get_or_insert_default();
    let mut frames: Vec<(String, String)> = components
        .schemas
        .iter()
        .filter_map(|(name, schema)| match schema {
            RefOr::T(Schema::Object(object)) => object
                .extensions
                .as_ref()?
                .get(EVENT_EXTENSION)?
                .as_str()
                .map(|event| (event.to_owned(), name.clone())),
            _ => None,
        })
        .collect();
    if frames.is_empty() {
        return;
    }
    frames.sort();
    let mut one_of = OneOfBuilder::new().description(Some(
        "A frame of the stream of server-sent events, one schema for each type, which its \
         `event` names (REALTIME-008).",
    ));
    for (_, component) in &frames {
        one_of = one_of.item(Ref::from_schema_name(component.as_str()));
    }
    components
        .schemas
        .insert(FRAME.to_owned(), RefOr::T(Schema::OneOf(one_of.build())));
    let reference = serde_json::json!({ "$ref": format!("#/components/schemas/{FRAME}") });
    for item in document.paths.paths.values_mut() {
        for (_, operation) in operations(item) {
            for response in operation.responses.responses.values_mut() {
                let RefOr::T(response) = response else {
                    continue;
                };
                if let Some(RefOr::T(content)) = response.content.get_mut(EVENT_STREAM) {
                    content
                        .extensions
                        .get_or_insert_with(Extensions::default)
                        .insert(ITEM_SCHEMA.to_owned(), reference.clone());
                }
            }
        }
    }
}

/// Each operation of `item`, with its method.
fn operations(item: &mut PathItem) -> Vec<(Method, &mut Operation)> {
    [
        (Method::GET, &mut item.get),
        (Method::PUT, &mut item.put),
        (Method::POST, &mut item.post),
        (Method::DELETE, &mut item.delete),
        (Method::OPTIONS, &mut item.options),
        (Method::HEAD, &mut item.head),
        (Method::PATCH, &mut item.patch),
        (Method::TRACE, &mut item.trace),
    ]
    .into_iter()
    .filter_map(|(method, operation)| operation.as_mut().map(|operation| (method, operation)))
    .collect()
}

fn query_names(operation: &Operation) -> HashSet<String> {
    operation
        .parameters
        .iter()
        .flatten()
        .filter_map(|parameter| match parameter {
            RefOr::T(parameter) if matches!(parameter.parameter_in, ParameterIn::Query) => {
                Some(parameter.name.clone())
            }
            _ => None,
        })
        .collect()
}

/// The problems the pipeline can answer on `operation`, and those of `errors`, which its module
/// answers, by status, each status with the headers its problems carry.
fn problems(
    method: &Method,
    access: Access,
    operation: &Operation,
    errors: &[&dyn ErrorCode],
) -> BTreeMap<String, Response> {
    let every = RequestError::every();
    let pipeline = every
        .iter()
        .filter(|error| answers(error, method, access, operation))
        .map(|error| error as &dyn ErrorCode);
    let mut by_status: BTreeMap<String, Vec<&dyn ErrorCode>> = BTreeMap::new();
    for error in pipeline.chain(errors.iter().copied()) {
        by_status
            .entry(error.category().status().as_u16().to_string())
            .or_default()
            .push(error);
    }
    by_status
        .into_iter()
        .map(|(status, errors)| {
            let mut response = problem_response(&errors);
            if status == "503" || status == "429" {
                response.headers.insert(
                    "retry-after".to_owned(),
                    header("How many seconds to wait before trying again.").into(),
                );
            }
            (status, response)
        })
        .collect()
}

fn problem_response(errors: &[&dyn ErrorCode]) -> Response {
    let codes: Vec<&str> = errors.iter().map(|error| error.code()).collect();
    let description = errors
        .iter()
        .map(|error| format!("`{}`: {}.", error.code(), error.title()))
        .collect::<Vec<_>>()
        .join(" ");
    let schema = AllOfBuilder::new()
        .item(Ref::from_schema_name(ProblemBody::name()))
        .item(
            ObjectBuilder::new().property(
                "code",
                ObjectBuilder::new()
                    .schema_type(Type::String)
                    .enum_values(Some(codes)),
            ),
        )
        .build();
    let mut response = Response::new(description);
    response.content.insert(
        PROBLEM_MEDIA_TYPE.to_owned(),
        Content::new(Some(Schema::AllOf(schema))).into(),
    );
    response
}

/// A response header whose value is text.
fn header(description: &str) -> utoipa::openapi::header::Header {
    HeaderBuilder::new()
        .schema(Some(ObjectBuilder::new().schema_type(Type::String)))
        .description(Some(description))
        .build()
}
