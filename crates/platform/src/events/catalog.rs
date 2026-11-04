//! The event catalog: what the OpenAPI document says of each frame type the stream of
//! server-sent events sends, each module's events and the stream's own frames (REALTIME-008).
//!
//! Each frame's schema describes the frame once a client's parser has read it, as OpenAPI 3.2
//! describes the items of a `text/event-stream`: its `event`, its `id`, and its `data`, a string
//! of JSON whose `contentSchema` holds the envelope of REALTIME-002 and the payload.

use std::{borrow::Cow, marker::PhantomData};

use serde::Serialize;
use utoipa::{
    ToSchema,
    openapi::{
        Components, KnownFormat, ObjectBuilder, Ref, RefOr, Schema, SchemaFormat, Type,
        extensions::ExtensionsBuilder, schema::SchemaType,
    },
};

use super::{UserEvent, WorkspaceEvent};
use crate::authz::Permission;

/// The extension that marks a frame type's schema among the components, with the type's name.
pub(crate) const EVENT_EXTENSION: &str = "x-event";

/// Whether a frame type's frames carry an `id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameId {
    /// Every frame has one: an event's ID.
    Always,
    /// Some frames have one, such as the `stream.opened` of a stream that a reconnect resumes.
    Sometimes,
    /// No frame has one.
    Never,
}

/// A frame type that the catalog documents: an event, or a frame the stream sends of its own.
pub trait Catalogued: Sync {
    /// The frame type's entry in the catalog.
    fn entry(&self) -> Entry;
}

/// What the catalog says of one frame type.
pub struct Entry {
    name: &'static str,
    description: Cow<'static, str>,
    id: FrameId,
    payload: String,
    schemas: Vec<(String, RefOr<Schema>)>,
}

impl Entry {
    fn new<T: ToSchema>(name: &'static str, description: Cow<'static, str>, id: FrameId) -> Self {
        let mut schemas = vec![(T::name().into_owned(), T::schema())];
        T::schemas(&mut schemas);
        Self {
            name,
            description,
            id,
            payload: T::name().into_owned(),
            schemas,
        }
    }

    /// Adds the frame type's schema to `components`, marked as a frame's, with its payload's
    /// schema and those it refers to.
    pub(crate) fn document(self, components: &mut Components) {
        let frame = frame_schema(&self);
        for (name, schema) in self.schemas {
            components.schemas.entry(name).or_insert(schema);
        }
        components.schemas.insert(component(self.name), frame);
    }
}

impl<T: Serialize + ToSchema> Catalogued for UserEvent<T> {
    fn entry(&self) -> Entry {
        Entry::new::<T>(
            self.name,
            Cow::Owned(format!(
                "`{}`, a user event: it reaches its user's streams.",
                self.name
            )),
            FrameId::Always,
        )
    }
}

impl<P: Permission, T: Serialize + ToSchema> Catalogued for WorkspaceEvent<P, T> {
    fn entry(&self) -> Entry {
        Entry::new::<T>(
            self.name,
            Cow::Owned(format!(
                "`{}`, a workspace event: it reaches the streams of the workspace's members who \
                 hold `{}` (SEC-19).",
                self.name,
                P::NAME
            )),
            FrameId::Always,
        )
    }
}

/// A frame that the stream sends of its own, beside the events, such as `stream.opened`, with
/// a payload of type `T`.
pub struct StreamFrame<T> {
    name: &'static str,
    id: FrameId,
    description: &'static str,
    payload: PhantomData<fn(&T)>,
}

impl<T: Serialize + ToSchema> StreamFrame<T> {
    /// The frame type `name`, whose frames carry an `id` as `id` says, and which `description`
    /// describes.
    pub const fn new(name: &'static str, id: FrameId, description: &'static str) -> Self {
        Self {
            name,
            id,
            description,
            payload: PhantomData,
        }
    }

    /// The frame type's name, its frames' `event`.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

impl<T: Serialize + ToSchema> Catalogued for StreamFrame<T> {
    fn entry(&self) -> Entry {
        Entry::new::<T>(self.name, Cow::Borrowed(self.description), self.id)
    }
}

/// The name of the component that holds the schema of the frame type `name`: `member.removed`'s
/// is `MemberRemovedFrame`.
fn component(name: &str) -> String {
    let mut component: String = name
        .split(['.', '_'])
        .map(|word| {
            let mut letters = word.chars();
            letters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(letters).collect()
            })
        })
        .collect();
    component.push_str("Frame");
    component
}

/// The schema of a frame of `entry`'s type, once a client's parser has read it.
fn frame_schema(entry: &Entry) -> RefOr<Schema> {
    let envelope = ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("data", Ref::from_schema_name(entry.payload.clone()))
        .required("data")
        .property(
            "trace_id",
            ObjectBuilder::new()
                .schema_type(SchemaType::from_iter([Type::String, Type::Null]))
                .description(Some(
                    "The trace ID of the request or job that published the event, or of the \
                     request that opened the stream, for a frame of the stream's own; null for \
                     an event published outside a trace (REALTIME-002).",
                )),
        )
        .required("trace_id");
    let data = ObjectBuilder::new()
        .schema_type(Type::String)
        .content_media_type("application/json")
        .content_schema(Some(envelope))
        .description(Some(
            "JSON: the payload, and the trace ID of what sent it (REALTIME-002).",
        ));
    let mut frame = ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some(entry.description.clone()))
        .property(
            "event",
            ObjectBuilder::new()
                .schema_type(Type::String)
                .enum_values(Some([entry.name])),
        )
        .required("event")
        .property("data", data)
        .required("data")
        .extensions(Some(
            ExtensionsBuilder::new()
                .add(EVENT_EXTENSION, entry.name)
                .build(),
        ));
    if entry.id != FrameId::Never {
        frame = frame.property(
            "id",
            ObjectBuilder::new()
                .schema_type(Type::String)
                .format(Some(SchemaFormat::KnownFormat(KnownFormat::Uuid)))
                .description(Some(
                    "What a client that reconnects names in `Last-Event-ID` (REALTIME-004).",
                )),
        );
        if entry.id == FrameId::Always {
            frame = frame.required("id");
        }
    }
    frame.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realtime_008_a_frame_type_s_component_is_named_after_it() {
        assert_eq!(component("member.removed"), "MemberRemovedFrame");
        assert_eq!(component("stream.opened"), "StreamOpenedFrame");
        assert_eq!(component("sign_in.failed"), "SignInFailedFrame");
    }
}
