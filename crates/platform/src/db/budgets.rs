//! The statement that counts bytes in a rate-limit bucket (PLATFORM-089).

use std::time::Duration;

use super::Database;
use crate::errors::Internal;

impl Database {
    /// Adds `bytes` to the rate-limit bucket keyed by `key_hash`, in one statement that commits
    /// at once, and returns the window's count and how long until the window ends. A window that
    /// has ended starts again with these bytes, all by the database's clock, as a window of
    /// requests does (PLATFORM-055).
    pub(crate) async fn spend(
        &self,
        key_hash: &[u8; 32],
        window: Duration,
        bytes: u32,
    ) -> Result<(u32, Duration), Internal> {
        let bytes = i32::try_from(bytes).unwrap_or(i32::MAX);
        let row = sqlx::query!(
            r#"INSERT INTO rate_limit_buckets AS bucket (key_hash, count, window_started_at)
            VALUES ($1, $3, now())
            ON CONFLICT (key_hash) DO UPDATE SET
                count = CASE
                    WHEN bucket.window_started_at + make_interval(secs => $2) <= now() THEN $3
                    ELSE LEAST(bucket.count, 2147483647 - $3) + $3
                END,
                window_started_at = CASE
                    WHEN bucket.window_started_at + make_interval(secs => $2) <= now() THEN now()
                    ELSE bucket.window_started_at
                END
            RETURNING
                count,
                extract(epoch FROM window_started_at + make_interval(secs => $2) - now())::float8
                    AS "resets_in!""#,
            key_hash.as_slice(),
            window.as_secs_f64(),
            bytes,
        )
        .fetch_one(&self.pool)
        .await?;
        let count = u32::try_from(row.count).unwrap_or(u32::MAX);
        let resets_in = Duration::try_from_secs_f64(row.resets_in.max(0.0)).unwrap_or(window);
        Ok((count, resets_in))
    }
}
