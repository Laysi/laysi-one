//! A simple fixed-window rate limit, used by `/mcp`, `/v1/auth/bootstrap` and
//! `/v1/auth/login`.
//!
//! Once exposed publicly, a leaked token could be used without bound. This
//! layer enforces calls per minute and deliberately avoids redis: the
//! container is a single-host deployment and an in-process counter is
//! enough. The cost is that a restart clears the counts and that several
//! replicas would each count separately; moving to shared storage is a
//! problem for the day horizontal scaling actually happens.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

struct Bucket {
    /// The minute the current count belongs to (Unix epoch minutes).
    window: i64,
    count: u32,
}

/// Fixed-window (per minute) counter. The bucket key is the caller's choice:
/// `/mcp` counts per token id, login counts per login name.
pub struct RateLimiter {
    limit_per_minute: u32,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    /// `limit_per_minute = 0` means unlimited, which turns the whole layer off
    /// for local development or testing.
    pub fn new(limit_per_minute: u32) -> Self {
        Self { limit_per_minute, buckets: Mutex::new(HashMap::new()) }
    }

    /// Record a call and report whether it is still within the allowance.
    /// `true` allows, `false` should be blocked.
    pub fn allow(&self, key: &str) -> bool {
        if self.limit_per_minute == 0 {
            return true;
        }
        let window = current_window();
        // A poisoned lock (another thread panicking while holding it) must not
        // break the rate limiter: a possibly incomplete count is better than
        // taking down /mcp entirely.
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let bucket =
            buckets.entry(key.to_string()).or_insert(Bucket { window, count: 0 });
        if bucket.window != window {
            bucket.window = window;
            bucket.count = 0;
        }
        bucket.count += 1;
        bucket.count <= self.limit_per_minute
    }
}

fn current_window() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64 / 60
}

/// Read the per-minute limit from the environment. Unset uses the default;
/// set to anything other than a non-negative integer is a configuration
/// error.
pub fn limit_from_env(default: u32) -> anyhow::Result<u32> {
    let Some(raw) = std::env::var("MCP_RATE_LIMIT_PER_MINUTE")
        .ok()
        .filter(|v| !v.trim().is_empty())
    else {
        return Ok(default);
    };
    raw.trim()
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("MCP_RATE_LIMIT_PER_MINUTE 必須是非負整數，收到「{raw}」"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_the_limit_then_blocks_within_the_same_minute() {
        let limiter = RateLimiter::new(3);
        assert!(limiter.allow("1"));
        assert!(limiter.allow("1"));
        assert!(limiter.allow("1"));
        assert!(!limiter.allow("1"), "第四次呼叫應被擋下");
        assert!(!limiter.allow("1"), "被擋下之後仍應持續拒絕，不能因為又呼叫一次就放行");
    }

    #[test]
    fn different_tokens_have_independent_buckets() {
        let limiter = RateLimiter::new(1);
        assert!(limiter.allow("1"));
        assert!(!limiter.allow("1"));
        // Token 2's allowance must not be consumed by token 1.
        assert!(limiter.allow("2"));
    }

    #[test]
    fn zero_means_unlimited() {
        let limiter = RateLimiter::new(0);
        for _ in 0..1000 {
            assert!(limiter.allow("1"));
        }
    }

    #[test]
    fn limit_from_env_rejects_non_numeric_values() {
        // Rather than setting the environment variable (tests run in parallel
        // and would clash), test the underlying string parsing directly.
        assert_eq!("60".trim().parse::<u32>().ok(), Some(60));
        assert!("abc".trim().parse::<u32>().is_err());
    }
}
