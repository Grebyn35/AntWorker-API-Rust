//! Budgets: buckets whose limit is in bytes, and whose windows count the bytes of a kind of
//! request, such as those of the bodies that a user sends to a route (PLATFORM-089). A budget's
//! name, limit and window are a [`Bucket`]'s.

use super::{Bucket, Count, LimitError, REFUSALS, RateLimiter, RateLimits};
use crate::errors::Internal;

impl RateLimiter {
    /// Counts `bytes` in the budget of `budget`'s kind for `value`, such as a user's ID, in one
    /// statement that commits at once, as [`RateLimiter::count`] counts a request (PLATFORM-055).
    /// A request that it refuses is counted in `platform.rate_limit_refusals`, by the budget's
    /// kind.
    async fn spend(&self, budget: Bucket, value: &[u8], bytes: u32) -> Result<Count, Internal> {
        let key = self.key.hash(budget.name, value);
        let (count, resets_in) = self.database.spend(&key, budget.window, bytes).await?;
        let count = Count {
            bucket: budget,
            count,
            resets_in,
            bytes: true,
        };
        if count.refused() {
            REFUSALS.add(1, [budget.name.into()]);
        }
        Ok(count)
    }
}

impl RateLimits {
    /// Counts `bytes` in the budget of `budget`'s kind for `value`, such as a user's ID, and
    /// refuses the request once they take the window's count beyond the budget's limit. The
    /// bytes of a refused request stay counted (PLATFORM-089). A budget joins the request's
    /// counts only when it refuses the request, so the headers of a response that no budget
    /// refused describe a bucket that counts requests (PLATFORM-056).
    pub async fn spend(&self, budget: Bucket, value: &[u8], bytes: u32) -> Result<(), LimitError> {
        let count = self.limiter.spend(budget, value, bytes).await?;
        if count.refused() {
            self.counted.push(count);
            return Err(LimitError::RateLimited);
        }
        Ok(())
    }
}
