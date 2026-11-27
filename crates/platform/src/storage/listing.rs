//! A listing of the bucket's objects, a page at a time, as S3's `ListObjectsV2` gives it
//! (PLATFORM-101).

use futures_util::{
    StreamExt,
    stream::{self, BoxStream},
};
use rusty_s3::{S3Action, actions::ListObjectsV2};
use url::Url;

use super::{
    Call, Listed, ObjectKey, Storage, StorageError, URL_LIFETIME,
    failures::{answered_with, sent},
};
use crate::{errors::Internal, types::Timestamp};

/// The most bytes of a page that the client reads: more than the 1,000 objects of a page take,
/// each with a key of S3's longest, 1,024 bytes, encoded, and, in a listing of versions, with a
/// version's ID of S3's longest, 1,024 bytes.
const PAGE_MAX_BYTES: usize = 8 << 20;

/// The pages of the listing of the objects whose keys start with `prefix`, each asked of
/// `storage`'s bucket once the one before it is read, until a page names none after it.
pub(super) fn pages(
    storage: Storage,
    prefix: String,
) -> BoxStream<'static, Result<Vec<Listed>, StorageError>> {
    // The token of the page to ask for: none for the first, and no page left once it is `None`.
    stream::unfold(Some(None::<String>), move |token| {
        let (storage, prefix) = (storage.clone(), prefix.clone());
        async move {
            let token = token?;
            match page(&storage, &prefix, token.as_deref()).await {
                Ok((listed, next)) => Some((Ok(listed), next.map(Some))),
                Err(error) => Some((Err(error), None)),
            }
        }
    })
    .boxed()
}

/// The page of the listing that `token` continues, or its first, and the token of the page after
/// it, if the listing goes on.
async fn page(
    storage: &Storage,
    prefix: &str,
    token: Option<&str>,
) -> Result<(Vec<Listed>, Option<String>), StorageError> {
    let url = {
        let mut action = storage.bucket.list_objects_v2(Some(&storage.credentials));
        action.with_prefix(prefix);
        if let Some(token) = token {
            action.with_continuation_token(token);
        }
        action.sign(URL_LIFETIME)
    };
    read(&fetched(storage, url, Call::List).await?)
}

/// The bytes of the page that `url` asks `storage`'s bucket for, of the listing `call`, which
/// fails on a page of more than `PAGE_MAX_BYTES`.
pub(super) async fn fetched(
    storage: &Storage,
    url: Url,
    call: Call,
) -> Result<Vec<u8>, StorageError> {
    let mut answer = answered_with(storage.client.get(url).send().await, call)?;
    let mut page = Vec::new();
    while let Some(chunk) = answer.chunk().await.map_err(|error| sent(&error, call))? {
        if page.len() + chunk.len() > PAGE_MAX_BYTES {
            return Err(unread(call, "a page of more than 8 MiB"));
        }
        page.extend_from_slice(&chunk);
    }
    Ok(page)
}

/// The objects that the page `xml` lists, and the token of the page after it, if the listing goes
/// on. The parser refuses a document type, so it resolves no entity of a document's own
/// (SEC-20). An object whose key no URL addresses, or whose time of writing the page doesn't
/// give as S3 writes it, fails the page: the deletion of unreferenced objects would act on it.
fn read(xml: &[u8]) -> Result<(Vec<Listed>, Option<String>), StorageError> {
    let text =
        std::str::from_utf8(xml).map_err(|_| unread(Call::List, "a page that isn't UTF-8"))?;
    let parsed = ListObjectsV2::parse_response(text)
        .map_err(|_| unread(Call::List, "a page that isn't a listing"))?;
    let mut listed = Vec::with_capacity(parsed.contents.len());
    for object in parsed.contents {
        let key = ObjectKey::from_stored(&object.key);
        if !key.is_addressed() {
            return Err(unread(
                Call::List,
                "a key with a segment that a URL's path doesn't keep",
            ));
        }
        let written_at = object
            .last_modified
            .as_deref()
            .and_then(|at| Timestamp::read(at).ok())
            .ok_or_else(|| {
                unread(
                    Call::List,
                    "an object without a time of writing that it reads",
                )
            })?;
        listed.push(Listed {
            key,
            written_at: written_at.at(),
        });
    }
    Ok((listed, parsed.next_continuation_token))
}

/// The failure of the listing `call` whose page holds `what`.
pub(super) fn unread(call: Call, what: &str) -> StorageError {
    StorageError::Failed(Internal::new(format!(
        "the bucket answered {} with {what}",
        call.operation()
    )))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;
    use crate::{clock::TestClock, storage::StorageConfig, types::Secret};

    /// What the client reads of a page of the listing `call` that a bucket on the loopback
    /// address answers with `page`, after the request's head.
    async fn fetched_from(page: Vec<u8>, call: Call) -> Result<Vec<u8>, StorageError> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            while stream.read_line(&mut line).await.unwrap_or(0) > 2 {
                line.clear();
            }
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", page.len());
            let stream = stream.get_mut();
            // A client that stops reading closes the connection, which ends the writes.
            if stream.write_all(head.as_bytes()).await.is_ok() {
                stream.write_all(&page).await.unwrap_or_default();
            }
        });
        let config = StorageConfig {
            endpoint: endpoint.parse().unwrap(),
            region: "us-east-1".to_owned(),
            bucket: "files".to_owned(),
            access_key_id: "an-id".to_owned(),
            secret_access_key: Secret::new("a-secret".to_owned()),
            path_style: true,
            ca_path: None,
        };
        let storage = Storage::new(&config, Arc::new(TestClock::default())).unwrap();
        fetched(&storage, endpoint.parse().unwrap(), call).await
    }

    /// Has a page of 8 MiB of the listing `call` read whole, and one of a byte more fail.
    async fn reads_8_mib_and_no_more(call: Call) {
        let most = 8 << 20;

        let read = fetched_from(vec![b' '; most], call).await;
        let over = fetched_from(vec![b' '; most + 1], call).await;

        assert_eq!(read.map(|page| page.len()).ok(), Some(most));
        assert!(
            matches!(over, Err(StorageError::Failed(_))),
            "{:?}",
            over.map(|page| page.len())
        );
    }

    #[tokio::test]
    async fn platform_101_a_page_of_8_mib_is_read_and_one_of_a_byte_more_fails() {
        reads_8_mib_and_no_more(Call::List).await;
    }

    #[tokio::test]
    async fn platform_102_a_page_of_8_mib_is_read_and_one_of_a_byte_more_fails() {
        reads_8_mib_and_no_more(Call::Versions).await;
    }

    /// A page that lists `objects`, each given as the elements of its `Contents`, and `rest`
    /// after them.
    fn listing(objects: &[&str], rest: &str) -> String {
        let contents: String = objects
            .iter()
            .map(|object| format!("<Contents>{object}</Contents>"))
            .collect();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult \
             xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <Name>files</Name>{contents}{rest}</ListBucketResult>"
        )
    }

    const WRITTEN: &str = "<LastModified>2026-11-19T11:48:17.000Z</LastModified>";

    #[test]
    fn platform_101_a_page_gives_its_objects_and_the_token_of_the_next() {
        let first = format!("<Key>avatars/0a</Key>{WRITTEN}<Size>3</Size>");
        let second = format!("<Key>avatars/a+b%25c</Key>{WRITTEN}");
        let more = "<IsTruncated>true</IsTruncated>\
                    <NextContinuationToken>avatars/0b</NextContinuationToken>\
                    <EncodingType>url</EncodingType>";

        let (listed, next) = read(listing(&[&first, &second], more).as_bytes()).unwrap();
        let (last, end) = read(listing(&[&first], "").as_bytes()).unwrap();

        let keys: Vec<&str> = listed.iter().map(|object| object.key.as_str()).collect();
        assert_eq!(keys, ["avatars/0a", "avatars/a b%c"]);
        assert_eq!(listed[0].written_at.unix_timestamp(), 1_795_088_897);
        assert_eq!(next.as_deref(), Some("avatars/0b"));
        assert_eq!((last.len(), end), (1, None));
    }

    #[test]
    fn platform_101_a_page_with_a_document_type_or_an_entity_of_its_own_fails() {
        let object = format!("<Key>a</Key>{WRITTEN}");
        let page = listing(&[&object], "");
        let typed = page.replacen(
            "<ListBucketResult",
            "<!DOCTYPE r [<!ENTITY x SYSTEM \"file:///etc/hostname\">]><ListBucketResult",
            1,
        );
        let entity = page.replacen("<Key>a</Key>", "<Key>a&x;</Key>", 1);

        assert!(read(page.as_bytes()).is_ok());
        for refused in [typed, entity] {
            assert!(
                matches!(read(refused.as_bytes()), Err(StorageError::Failed(_))),
                "{refused}"
            );
        }
    }

    #[test]
    fn platform_101_a_page_outside_s3_s_namespace_fails() {
        let page = listing(&[&format!("<Key>a</Key>{WRITTEN}")], "");
        let s3 = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";
        let other = page.replacen(s3, "xmlns=\"http://example.com/doc/2006-03-01/\"", 1);
        let unnamed = page.replacen(s3, "", 1);

        assert!(read(page.as_bytes()).is_ok());
        for refused in [other, unnamed] {
            assert!(
                matches!(read(refused.as_bytes()), Err(StorageError::Failed(_))),
                "{refused}"
            );
        }
    }

    #[test]
    fn platform_101_a_page_fails_on_a_key_no_url_addresses_and_on_a_time_it_can_t_read() {
        for object in [
            format!("<Key>avatars/../pictures/0a</Key>{WRITTEN}"),
            format!("<Key>avatars//0a</Key>{WRITTEN}"),
            "<Key>avatars/0a</Key>".to_owned(),
            "<Key>avatars/0a</Key><LastModified>yesterday</LastModified>".to_owned(),
            "<Key>avatars/0a</Key><LastModified>2026-11-19T11:48:17+00:00</LastModified>"
                .to_owned(),
        ] {
            let page = listing(&[&object], "");
            assert!(
                matches!(read(page.as_bytes()), Err(StorageError::Failed(_))),
                "{object}"
            );
        }
        // A page that is cut short, and one that says more follows without saying where.
        let whole = listing(&[&format!("<Key>a</Key>{WRITTEN}")], "");
        let cut = &whole[..whole.len() - 10];
        let lost = listing(&[], "<IsTruncated>true</IsTruncated>");
        for refused in [cut, &lost, "", "not a listing"] {
            assert!(
                matches!(read(refused.as_bytes()), Err(StorageError::Failed(_))),
                "{refused}"
            );
        }
    }
}
