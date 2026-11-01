//! Rate limits: buckets that count requests in windows, in the database that every replica shares
//! (PLATFORM-026, PLATFORM-048 to PLATFORM-051).

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::{
    db::Database,
    errors::{Category, ErrorCode, Internal},
    hashing::HmacKey,
    telemetry::{metrics::Counter, names},
};

static REFUSALS: Counter<1> = Counter::new(
    "platform.rate_limit_refusals",
    "{request}",
    "Requests that a rate limit refused, by the kind of the bucket that refused them (PLATFORM-048).",
    [names::RATE_LIMIT_BUCKET],
);

/// A kind of bucket: its name, which the rate-limit headers carry, and how many requests it
/// allows in each window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bucket {
    /// The name, such as `ip` or `sign_in` (PLATFORM-050).
    pub name: &'static str,
    /// How many requests a window allows.
    pub limit: u32,
    /// The window's length.
    pub window: Duration,
}

/// A bucket's state after it counted a request.
#[derive(Clone, Copy, Debug)]
pub struct Count {
    /// The bucket that counted.
    pub bucket: Bucket,
    /// The requests its window has counted, this one included.
    pub count: u32,
    /// How long until its window ends.
    pub resets_in: Duration,
}

impl Count {
    /// Whether the request is beyond the bucket's limit.
    pub fn refused(&self) -> bool {
        self.count > self.bucket.limit
    }

    /// The requests the window still allows.
    pub fn remaining(&self) -> u32 {
        self.bucket.limit.saturating_sub(self.count)
    }
}

/// Counts requests in buckets keyed by an HMAC of what they count (PLATFORM-051).
#[derive(Clone)]
pub struct RateLimiter {
    database: Database,
    key: HmacKey,
}

impl RateLimiter {
    /// A limiter that counts in `database`, keying buckets with `key`.
    pub fn new(database: Database, key: HmacKey) -> Self {
        Self { database, key }
    }

    /// Counts one request in the bucket of `bucket`'s kind for `value`, such as a client IP's,
    /// in one statement that commits at once (PLATFORM-026, PLATFORM-049). A request it refuses
    /// is counted in `platform.rate_limit_refusals`, by the bucket's kind.
    pub async fn count(&self, bucket: Bucket, value: &[u8]) -> Result<Count, Internal> {
        let key = self.key.hash(bucket.name, value);
        let (count, resets_in) = self.database.count(&key, bucket.window).await?;
        let count = Count {
            bucket,
            count,
            resets_in,
        };
        if count.refused() {
            REFUSALS.add(1, [bucket.name.into()]);
        }
        Ok(count)
    }
}

/// The buckets that counted one request, which the pipeline describes in the response's headers
/// (PLATFORM-050).
#[derive(Clone, Debug, Default)]
pub struct Counted(Arc<Mutex<Vec<Count>>>);

impl Counted {
    /// Records that `count`'s bucket counted the request.
    pub fn push(&self, count: Count) {
        self.counts().push(count);
    }

    /// The bucket that decided the response: one that refused the request if any did, and
    /// otherwise the one with the fewest requests left; of those that tie, the one whose window
    /// ends last, so a client that waits for it waits long enough (PLATFORM-050).
    pub fn tightest(&self) -> Option<Count> {
        self.counts().iter().copied().min_by(|a, b| {
            b.refused()
                .cmp(&a.refused())
                .then(a.remaining().cmp(&b.remaining()))
                .then(b.resets_in.cmp(&a.resets_in))
        })
    }

    /// The counts. A lock that a panic poisoned still holds whole counts, since a push only
    /// appends, so its data is taken as it is.
    fn counts(&self) -> std::sync::MutexGuard<'_, Vec<Count>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The rate limits a module checks for one request, such as the sign-in email's (AUTH-006). Each
/// count joins the request's [`Counted`], so its headers can describe it.
#[derive(Clone)]
pub struct RateLimits {
    limiter: RateLimiter,
    counted: Counted,
}

impl RateLimits {
    /// The limits of a request that `counted` records.
    pub fn new(limiter: RateLimiter, counted: Counted) -> Self {
        Self { limiter, counted }
    }

    /// Counts the request in the bucket of `bucket`'s kind for `value`, and refuses it once the
    /// bucket is full (PLATFORM-026).
    pub async fn check(&self, bucket: Bucket, value: &[u8]) -> Result<(), LimitError> {
        let count = self.limiter.count(bucket, value).await?;
        self.counted.push(count);
        if count.refused() {
            return Err(LimitError::RateLimited);
        }
        Ok(())
    }
}

/// A rate limit refused the request, or couldn't count it.
#[derive(Debug, thiserror::Error)]
pub enum LimitError {
    /// The bucket is full (PLATFORM-048).
    #[error("a rate limit refused the request")]
    RateLimited,
    /// The count failed.
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl ErrorCode for LimitError {
    fn code(&self) -> &'static str {
        match self {
            Self::RateLimited => "request.rate_limited",
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::RateLimited => "Too many requests",
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::RateLimited => Category::RateLimited,
            Self::Internal(_) => Category::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUCKET: Bucket = Bucket {
        name: "ip",
        limit: 10,
        window: Duration::from_secs(60),
    };

    fn count(bucket: Bucket, count: u32, resets_in: u64) -> Count {
        Count {
            bucket,
            count,
            resets_in: Duration::from_secs(resets_in),
        }
    }

    #[test]
    fn platform_050_the_tightest_bucket_has_the_fewest_requests_left() {
        let counted = Counted::default();
        let user = Bucket {
            name: "user",
            limit: 5,
            ..BUCKET
        };
        counted.push(count(BUCKET, 3, 40));
        counted.push(count(user, 2, 50));
        let tightest = counted.tightest().unwrap();
        assert_eq!(tightest.bucket.name, "user");
        assert_eq!(tightest.remaining(), 3);
    }

    #[test]
    fn platform_050_a_bucket_that_refused_decides_over_one_merely_full() {
        let counted = Counted::default();
        counted.push(count(BUCKET, 10, 600));
        counted.push(count(
            Bucket {
                name: "user",
                ..BUCKET
            },
            11,
            5,
        ));
        assert_eq!(counted.tightest().unwrap().bucket.name, "user");
    }

    #[test]
    fn platform_050_of_two_full_buckets_the_tightest_ends_last() {
        let counted = Counted::default();
        counted.push(count(BUCKET, 11, 10));
        counted.push(count(
            Bucket {
                name: "sign_in",
                ..BUCKET
            },
            12,
            600,
        ));
        assert_eq!(counted.tightest().unwrap().bucket.name, "sign_in");
    }
}
