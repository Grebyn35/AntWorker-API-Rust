//! What the bucket keeps of each object that was written, replaced or deleted in it: its
//! versions, as S3's `ListObjectVersions` lists them, a page at a time, and the bytes of one
//! (PLATFORM-096). A bucket keeps the versions of an object that was replaced or deleted only
//! while its versioning is on.

use futures_util::{
    StreamExt,
    stream::{self, BoxStream},
};
use instant_xml::FromXml;
use percent_encoding::percent_decode_str;
use reqwest::header::CONTENT_TYPE;
use rusty_s3::S3Action;

use super::{
    Call, Object, ObjectKey, Storage, StorageError, URL_LIFETIME,
    listing::{fetched, unread},
};
use crate::types::Timestamp;

/// S3's namespace.
const NAMESPACE: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The media type of a version that the bucket stores none for.
const NO_MEDIA_TYPE: &str = "application/octet-stream";

/// What the bucket keeps of one write of an object, or of one deletion of it by its name, as a
/// listing of versions gives it.
#[derive(Clone, Debug)]
pub struct Version {
    /// The object's key, as the bucket lists it: a request's URL may name no such object
    /// ([`ObjectKey::is_addressed`]).
    pub key: ObjectKey,
    /// The ID under which the bucket keeps it.
    pub id: String,
    /// When the bucket wrote it, by the bucket's clock.
    pub written_at: time::OffsetDateTime,
    /// Whether the bucket lists it as the object's latest: the bytes that a read of the object
    /// by its name finds, or the deletion after which such a read finds none.
    pub latest: bool,
    /// The bytes that it holds, or `None`, when it marks a deletion.
    pub content: Option<Content>,
}

/// The bytes that a version holds, as a listing of versions describes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Content {
    /// Their entity tag, as the bucket gives it.
    pub etag: Option<String>,
    /// How many they are.
    pub size: u64,
}

/// A version's bytes, as the bucket streams them, with the media type that it stores for them.
pub struct VersionBytes {
    /// The media type that the version was written with, or `application/octet-stream` when
    /// the bucket's answer names none.
    pub media_type: String,
    /// Its bytes, and how many they are.
    pub object: Object,
}

impl Storage {
    /// What the bucket keeps of the objects whose keys start with `prefix`, as it lists it, a
    /// page at a time: the versions of an object together, the newest first, as S3 orders them.
    pub fn versions(&self, prefix: &str) -> BoxStream<'static, Result<Version, StorageError>> {
        self.listed(pages(self.clone(), prefix.to_owned()), Call::Versions)
    }

    /// The bytes of the version `id` of the object `key`, as the bucket streams them, as
    /// [`Storage::get`] streams an object's.
    pub async fn version(&self, key: &ObjectKey, id: &str) -> Result<VersionBytes, StorageError> {
        let request = key.addressed().map(|object| {
            let mut action = self.bucket.get_object(Some(&self.credentials), object);
            action.query_mut().insert("versionId", id);
            self.client.get(action.sign(URL_LIFETIME))
        });
        let found = self.answer(Call::Get, request).await?;
        Ok(VersionBytes {
            media_type: media_type(found.headers()),
            object: self.streamed(found, false)?,
        })
    }
}

/// The media type that the `Content-Type` among `headers`, those of the bucket's answer to a
/// read, gives, or `NO_MEDIA_TYPE` when they hold none that is text.
fn media_type(headers: &reqwest::header::HeaderMap) -> String {
    headers
        .get(CONTENT_TYPE)
        .and_then(|media_type| media_type.to_str().ok())
        .unwrap_or(NO_MEDIA_TYPE)
        .to_owned()
}

/// The last version that a page lists, which the page after it starts after.
struct Marker {
    key: String,
    id: String,
}

/// The pages of the listing of the versions of the objects whose keys start with `prefix`, each
/// asked of `storage`'s bucket once the one before it is read, until a page names none after it.
fn pages(
    storage: Storage,
    prefix: String,
) -> BoxStream<'static, Result<Vec<Version>, StorageError>> {
    // The marker of the page to ask for: none for the first, and no page left once it is `None`.
    stream::unfold(Some(None::<Marker>), move |marker| {
        let (storage, prefix) = (storage.clone(), prefix.clone());
        async move {
            let marker = marker?;
            match page(&storage, &prefix, marker.as_ref()).await {
                Ok((versions, next)) => Some((Ok(versions), next.map(Some))),
                Err(error) => Some((Err(error), None)),
            }
        }
    })
    .boxed()
}

/// The page of the listing that starts after `marker`, or its first, and the marker of the page
/// after it, if the listing goes on.
async fn page(
    storage: &Storage,
    prefix: &str,
    marker: Option<&Marker>,
) -> Result<(Vec<Version>, Option<Marker>), StorageError> {
    let url = {
        // `rusty-s3` has no action for `ListObjectVersions`, whose request is a listing's, of
        // the bucket itself, with `versions` in place of the `list-type` of `ListObjectsV2`.
        let mut action = storage.bucket.list_objects_v2(Some(&storage.credentials));
        action.query_mut().remove("list-type");
        action.query_mut().insert("versions", "");
        action.with_prefix(prefix);
        if let Some(marker) = marker {
            action.query_mut().insert("key-marker", &*marker.key);
            action.query_mut().insert("version-id-marker", &*marker.id);
        }
        action.sign(URL_LIFETIME)
    };
    read(&fetched(storage, url, Call::Versions).await?)
}

/// A page of S3's `ListVersionsResult`. Its versions and its delete markers are one list, so they
/// keep the order in which the page gives them.
#[derive(FromXml)]
#[xml(rename = "ListVersionsResult", ns(NAMESPACE))]
struct Page {
    #[xml(rename = "IsTruncated")]
    is_truncated: Option<bool>,
    #[xml(rename = "NextKeyMarker")]
    next_key_marker: Option<String>,
    #[xml(rename = "NextVersionIdMarker")]
    next_version_id_marker: Option<String>,
    #[xml(rename = "EncodingType")]
    encoding_type: Option<String>,
    entries: Vec<Entry>,
}

/// A version that holds bytes, or one that marks a deletion.
#[derive(FromXml)]
#[xml(forward)]
enum Entry {
    Written(Written),
    Deleted(Deleted),
}

#[derive(FromXml)]
#[xml(rename = "Version", ns(NAMESPACE))]
struct Written {
    #[xml(rename = "Key")]
    key: String,
    #[xml(rename = "VersionId")]
    version_id: String,
    #[xml(rename = "IsLatest")]
    is_latest: bool,
    #[xml(rename = "LastModified")]
    last_modified: String,
    #[xml(rename = "ETag")]
    etag: Option<String>,
    #[xml(rename = "Size")]
    size: u64,
}

#[derive(FromXml)]
#[xml(rename = "DeleteMarker", ns(NAMESPACE))]
struct Deleted {
    #[xml(rename = "Key")]
    key: String,
    #[xml(rename = "VersionId")]
    version_id: String,
    #[xml(rename = "IsLatest")]
    is_latest: bool,
    #[xml(rename = "LastModified")]
    last_modified: String,
}

/// The versions that the page `xml` lists, in its order, and the marker of the page after it, if
/// the listing goes on. The parser refuses a document type, so it resolves no entity of a
/// document's own (SEC-20). A version without a time of writing as S3 writes one fails the
/// page. A page says whether more follows with `IsTruncated`, and one without it says that more
/// does by naming the key that the next page starts after; a page that more follows fails
/// unless it names both that key and the version: with the key alone, the next page would leave
/// out the rest of that key's versions.
fn read(xml: &[u8]) -> Result<(Vec<Version>, Option<Marker>), StorageError> {
    let unread = |what| unread(Call::Versions, what);
    let text = std::str::from_utf8(xml).map_err(|_| unread("a page that isn't UTF-8"))?;
    let page: Page = instant_xml::from_str(text)
        .map_err(|_| unread("a page that isn't a listing of versions"))?;
    // A bucket that encodes the keys, as the request asks, says so, and one that doesn't gives
    // them as they are.
    let encoded = page.encoding_type.as_deref() == Some("url");
    let key = |key: String| {
        if encoded {
            decoded(&key).ok_or_else(|| unread("a key that it doesn't decode"))
        } else {
            Ok(key)
        }
    };
    let mut versions = Vec::with_capacity(page.entries.len());
    for entry in page.entries {
        let (listed, id, latest, written, content) = match entry {
            Entry::Written(version) => (
                version.key,
                version.version_id,
                version.is_latest,
                version.last_modified,
                Some(Content {
                    etag: version.etag,
                    size: version.size,
                }),
            ),
            Entry::Deleted(marker) => (
                marker.key,
                marker.version_id,
                marker.is_latest,
                marker.last_modified,
                None,
            ),
        };
        let written_at = Timestamp::read(&written)
            .map_err(|_| unread("a version without a time of writing that it reads"))?
            .at();
        versions.push(Version {
            key: ObjectKey::from_stored(&key(listed)?),
            id,
            written_at,
            latest,
            content,
        });
    }
    let filled = |marker: Option<String>| marker.filter(|marker| !marker.is_empty());
    let (next_key, next_id) = (
        filled(page.next_key_marker),
        filled(page.next_version_id_marker),
    );
    let next = match (
        page.is_truncated.unwrap_or(next_key.is_some()),
        next_key,
        next_id,
    ) {
        (false, _, _) => None,
        (true, Some(listed), Some(id)) => Some(Marker {
            key: key(listed)?,
            id,
        }),
        (true, _, _) => {
            return Err(unread(
                "a page that more follows, without the key and the version that it starts after",
            ));
        }
    };
    Ok((versions, next))
}

/// `value`, which a bucket encoded for a URL, decoded: a `+` as a space, as S3 writes one, and
/// each `%` with its two digits as their byte; or `None`, when the bytes aren't UTF-8.
fn decoded(value: &str) -> Option<String> {
    let spaced = value.replace('+', " ");
    percent_decode_str(&spaced)
        .decode_utf8()
        .ok()
        .map(std::borrow::Cow::into_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page that holds `entries`, each a `Version` or a `DeleteMarker` element, and `rest`
    /// after them.
    fn listing(entries: &[String], rest: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListVersionsResult \
             xmlns=\"{NAMESPACE}\"><Name>files</Name><Prefix>users/</Prefix>\
             <MaxKeys>1000</MaxKeys>{}{rest}</ListVersionsResult>",
            entries.concat()
        )
    }

    /// The elements that a version and a delete marker share: the key `key`, the ID `id`,
    /// whether it is the latest, and the second `second` of one minute as its time of writing.
    fn shared(key: &str, id: &str, latest: bool, second: u8) -> String {
        format!(
            "<Key>{key}</Key><VersionId>{id}</VersionId><IsLatest>{latest}</IsLatest>\
             <Owner><ID>admin</ID><DisplayName>admin</DisplayName></Owner>\
             <LastModified>2026-11-20T12:32:{second:02}.000Z</LastModified>"
        )
    }

    /// A version of `size` bytes, as a bucket lists one, its entity tag's quotes escaped.
    fn version(key: &str, id: &str, latest: bool, second: u8, size: u64) -> String {
        format!(
            "<Version>{}<ETag>&#34;tag-{id}&#34;</ETag><Size>{size}</Size>\
             <StorageClass>STANDARD</StorageClass></Version>",
            shared(key, id, latest, second)
        )
    }

    /// A delete marker, as a bucket lists one.
    fn marker(key: &str, id: &str, latest: bool, second: u8) -> String {
        format!(
            "<DeleteMarker>{}</DeleteMarker>",
            shared(key, id, latest, second)
        )
    }

    #[test]
    fn platform_096_a_page_gives_versions_and_delete_markers_in_the_order_it_lists_them() {
        let page = listing(
            &[
                version("users/aa", "3", true, 21, 7),
                marker("users/aa", "2", false, 21),
                version("users/aa", "1", false, 20, 5),
                marker("users/bb", "5", true, 22),
                version("users/bb", "4", false, 21, 3),
            ],
            "<IsTruncated>false</IsTruncated>",
        );

        let (versions, next) = read(page.as_bytes()).unwrap();

        let read: Vec<_> = versions
            .iter()
            .map(|version| {
                (
                    version.key.as_str(),
                    version.id.as_str(),
                    version.latest,
                    version.content.as_ref().map(|content| content.size),
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                ("users/aa", "3", true, Some(7)),
                ("users/aa", "2", false, None),
                ("users/aa", "1", false, Some(5)),
                ("users/bb", "5", true, None),
                ("users/bb", "4", false, Some(3)),
            ]
        );
        assert_eq!(versions[0].written_at.unix_timestamp(), 1_795_177_941);
        assert_eq!(
            versions[0].content,
            Some(Content {
                etag: Some("\"tag-3\"".to_owned()),
                size: 7
            })
        );
        assert!(next.is_none());
    }

    #[test]
    fn platform_096_a_page_that_more_follows_names_the_key_and_the_version_it_starts_after() {
        let entries = [version("users/aa", "3", true, 21, 7)];
        let more = "<IsTruncated>true</IsTruncated><NextKeyMarker>users/aa</NextKeyMarker>\
                    <NextVersionIdMarker>3</NextVersionIdMarker>";
        let whole = "<IsTruncated>false</IsTruncated><NextKeyMarker>users/aa</NextKeyMarker>\
                     <NextVersionIdMarker>3</NextVersionIdMarker>";

        // A page that doesn't say whether more follows, and names where the next starts.
        let unsaid = "<NextKeyMarker>users/aa</NextKeyMarker>\
                      <NextVersionIdMarker>3</NextVersionIdMarker>";

        let (_, next) = read(listing(&entries, more).as_bytes()).unwrap();
        let (_, end) = read(listing(&entries, whole).as_bytes()).unwrap();
        let (_, named) = read(listing(&entries, unsaid).as_bytes()).unwrap();
        let (_, last) = read(listing(&entries, "").as_bytes()).unwrap();

        for next in [next, named] {
            let next = next.unwrap();
            assert_eq!((next.key.as_str(), next.id.as_str()), ("users/aa", "3"));
        }
        assert!(end.is_none());
        assert!(last.is_none());
        for lost in [
            "<IsTruncated>true</IsTruncated>",
            "<IsTruncated>true</IsTruncated><NextKeyMarker>users/aa</NextKeyMarker>",
            "<IsTruncated>true</IsTruncated><NextKeyMarker>users/aa</NextKeyMarker>\
             <NextVersionIdMarker></NextVersionIdMarker>",
            "<IsTruncated>true</IsTruncated><NextVersionIdMarker>3</NextVersionIdMarker>",
            "<NextKeyMarker>users/aa</NextKeyMarker>",
        ] {
            assert!(
                matches!(
                    read(listing(&entries, lost).as_bytes()),
                    Err(StorageError::Failed(_))
                ),
                "{lost}"
            );
        }
    }

    #[test]
    fn platform_096_a_version_s_media_type_is_its_answer_s_or_that_of_bytes_of_no_type() {
        let mut headers = reqwest::header::HeaderMap::new();
        let none = media_type(&headers);
        headers.insert(CONTENT_TYPE, "image/png".parse().unwrap());
        let stored = media_type(&headers);
        let not_text = reqwest::header::HeaderValue::from_bytes(b"image/\xff").unwrap();
        headers.insert(CONTENT_TYPE, not_text);

        assert_eq!(none, "application/octet-stream");
        assert_eq!(stored, "image/png");
        assert_eq!(media_type(&headers), "application/octet-stream");
    }

    #[test]
    fn platform_096_a_page_s_keys_are_decoded_only_when_it_says_that_it_encoded_them() {
        let entries = [version("users/a+b%2Bc%25d", "1", true, 21, 1)];
        let more = "<IsTruncated>true</IsTruncated><NextKeyMarker>users/a+b%2Bc%25d\
                    </NextKeyMarker><NextVersionIdMarker>1</NextVersionIdMarker>";
        let encoded = format!("{more}<EncodingType>url</EncodingType>");

        let (as_listed, after) = read(listing(&entries, more).as_bytes()).unwrap();
        let (decoded, next) = read(listing(&entries, &encoded).as_bytes()).unwrap();

        assert_eq!(as_listed[0].key.as_str(), "users/a+b%2Bc%25d");
        assert_eq!(after.unwrap().key, "users/a+b%2Bc%25d");
        assert_eq!(decoded[0].key.as_str(), "users/a b+c%d");
        assert_eq!(next.unwrap().key, "users/a b+c%d");
        let not_text = [version("users/%ff", "1", true, 21, 1)];
        let page = listing(&not_text, "<EncodingType>url</EncodingType>");
        assert!(matches!(
            read(page.as_bytes()),
            Err(StorageError::Failed(_))
        ));
    }

    #[test]
    fn platform_096_a_page_gives_a_key_that_no_url_addresses_as_the_bucket_lists_it() {
        let page = listing(&[version("users//aa", "1", true, 21, 1)], "");

        let (versions, _) = read(page.as_bytes()).unwrap();

        assert_eq!(versions[0].key.as_str(), "users//aa");
        assert!(!versions[0].key.is_addressed());
    }

    #[test]
    fn platform_096_a_page_fails_on_what_it_can_t_read() {
        let whole = listing(&[version("users/aa", "1", true, 21, 1)], "");
        let typed = whole.replacen(
            "<ListVersionsResult",
            "<!DOCTYPE r [<!ENTITY x SYSTEM \"file:///etc/hostname\">]><ListVersionsResult",
            1,
        );
        let entity = whole.replacen("users/aa", "users/aa&x;", 1);
        let other_time = whole.replacen("12:32:21.000Z", "12:32:21+00:00", 1);
        let no_time = whole.replacen(
            "<LastModified>2026-11-20T12:32:21.000Z</LastModified>",
            "",
            1,
        );
        let no_id = whole.replacen("<VersionId>1</VersionId>", "", 1);
        let no_size = whole.replacen("<Size>1</Size>", "", 1);
        let objects = whole.replace("ListVersionsResult", "ListBucketResult");
        let cut = &whole[..whole.len() - 10];

        assert!(read(whole.as_bytes()).is_ok());
        for refused in [
            typed.as_str(),
            &entity,
            &other_time,
            &no_time,
            &no_id,
            &no_size,
            &objects,
            cut,
            "",
            "not a listing",
        ] {
            assert!(
                matches!(read(refused.as_bytes()), Err(StorageError::Failed(_))),
                "{refused}"
            );
        }
    }
}
