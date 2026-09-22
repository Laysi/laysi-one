//! A simple fixed-window rate limit, used by `/mcp`, `/v1/auth/bootstrap` and
//! `/v1/auth/login`.
//!
//! Once exposed publicly, a leaked token could be used without bound. This
//! layer enforces calls per minute and deliberately avoids redis: the
//! container is a single-host deployment and an in-process counter is
//! enough. The cost is that a restart clears the counts and that several
//! replicas would each count separately; moving to shared storage is a
//! problem for the day horizontal scaling actually happens.
//!
//! `license-server/src/ratelimit.rs` is a copy of this file for console
//! sign-in; a fix here belongs there too.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

struct Bucket {
    /// The minute the current count belongs to (Unix epoch minutes).
    window: i64,
    count: u32,
}

/// Where a limiter's allowance comes from.
///
/// `/mcp` reads it fresh on every call because it is a setting on a screen,
/// and a limit that only took effect after a restart would be a setting the
/// screen lies about. Login's is a constant of the login flow.
enum Allowance {
    Fixed(u32),
    Live(fn() -> u32),
}

/// The most buckets the map holds.
///
/// The key is the caller's choice and login's is the submitted login name, so
/// an attacker naming a different account on every attempt adds one entry per
/// attempt. At this size the map first drops buckets from earlier minutes,
/// which changes no decision: their next use would reset the count anyway. If
/// the current minute alone fills it, the bucket with the lowest count goes.
/// Refusing the new key instead would lock out every login name not yet seen
/// this minute. A flooded name holds a count of one while the account being
/// guessed holds the limit, so the flood evicts its own buckets first.
///
/// Eviction can still reset a bucket, but never cheaply. To evict the account
/// being guessed at count c, a flood has to lift every other bucket to c
/// first: about c × `MAX_BUCKETS` login attempts, and every one the limiter
/// admits, up to the limit per name, costs an Argon2id verification. The next
/// minute resets that bucket for free, so eviction only buys an attacker more
/// than the limit per minute at a volume that has already exhausted the CPU.
/// A bounded map that refuses nobody cannot also promise never to reset.
const MAX_BUCKETS: usize = 10_000;

/// The longest key stored, in bytes.
///
/// Login's key arrives in the request body, so its length is bounded only by
/// the body limit. Longer keys are cut at a character boundary; two names that
/// share this prefix then share a bucket, which is stricter, not looser. An
/// ASCII email address is at most 254 bytes and never reaches the cut.
const MAX_KEY_BYTES: usize = 256;

/// Fixed-window (per minute) counter. The bucket key is the caller's choice:
/// `/mcp` counts per token id, login counts per login name.
pub struct RateLimiter {
    allowance: Allowance,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    /// `limit_per_minute = 0` means unlimited, which turns the whole layer off
    /// for local development or testing.
    pub fn new(limit_per_minute: u32) -> Self {
        Self {
            allowance: Allowance::Fixed(limit_per_minute),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// A limiter whose allowance is whatever the setting currently says.
    pub fn from_setting(read: fn() -> u32) -> Self {
        Self {
            allowance: Allowance::Live(read),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    fn limit_per_minute(&self) -> u32 {
        match self.allowance {
            Allowance::Fixed(n) => n,
            Allowance::Live(read) => read(),
        }
    }

    /// Record a call and report whether it is still within the allowance.
    /// `true` allows, `false` should be blocked.
    pub fn allow(&self, key: &str) -> bool {
        self.allow_in(key, current_window())
    }

    fn allow_in(&self, key: &str, window: i64) -> bool {
        let limit_per_minute = self.limit_per_minute();
        if limit_per_minute == 0 {
            return true;
        }
        let mut end = key.len().min(MAX_KEY_BYTES);
        while !key.is_char_boundary(end) {
            end -= 1;
        }
        let key = &key[..end];
        // A poisoned lock (another thread panicking while holding it) must not
        // break the rate limiter: a possibly incomplete count is better than
        // taking down /mcp entirely.
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if !buckets.contains_key(key) && buckets.len() >= MAX_BUCKETS {
            buckets.retain(|_, b| b.window == window);
            if buckets.len() >= MAX_BUCKETS {
                let lightest = buckets
                    .iter()
                    .min_by_key(|(_, b)| b.count)
                    .map(|(k, _)| k.clone());
                if let Some(lightest) = lightest {
                    buckets.remove(&lightest);
                }
            }
        }
        let bucket = buckets
            .entry(key.to_string())
            .or_insert(Bucket { window, count: 0 });
        if bucket.window != window {
            bucket.window = window;
            bucket.count = 0;
        }
        bucket.count += 1;
        bucket.count <= limit_per_minute
    }
}

fn current_window() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        / 60
}

/// Read the per-minute limit from the environment. Unset uses the default;
/// set to anything other than a non-negative integer is a configuration
/// error.
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
        assert!(
            !limiter.allow("1"),
            "被擋下之後仍應持續拒絕，不能因為又呼叫一次就放行"
        );
    }

    #[test]
    fn different_tokens_have_independent_buckets() {
        let limiter = RateLimiter::new(1);
        assert!(limiter.allow("1"));
        assert!(!limiter.allow("1"));
        // Token 2's allowance must not be consumed by token 1.
        assert!(limiter.allow("2"));
    }

    /// One entry per attempted login name would otherwise accumulate for the
    /// life of the process.
    #[test]
    fn buckets_from_an_earlier_minute_are_swept() {
        let limiter = RateLimiter::new(1);
        {
            let mut buckets = limiter.buckets.lock().expect("測試中的鎖不會被毒化");
            for i in 0..MAX_BUCKETS {
                buckets.insert(
                    format!("舊 {i}"),
                    Bucket {
                        window: 0,
                        count: 1,
                    },
                );
            }
        }

        assert!(limiter.allow("新"));
        let buckets = limiter.buckets.lock().expect("測試中的鎖不會被毒化");
        assert_eq!(buckets.len(), 1, "上一分鐘的桶應被清掉，只留這一次的");
    }

    #[test]
    fn distinct_keys_within_one_minute_stop_at_the_cap() {
        let limiter = RateLimiter::new(3);
        for _ in 0..4 {
            limiter.allow_in("被猜", 7);
        }
        for i in 0..MAX_BUCKETS + 100 {
            limiter.allow_in(&format!("名 {i}"), 7);
        }
        assert_eq!(
            limiter.buckets.lock().expect("測試中的鎖不會被毒化").len(),
            MAX_BUCKETS
        );
        assert!(!limiter.allow_in("被猜", 7), "灌名稱不能把被猜的帳號歸零");
        assert!(limiter.allow_in("新", 7), "表滿時新的鍵仍要收得進來");
    }

    #[test]
    fn a_long_key_is_stored_cut_at_a_character_boundary() {
        let limiter = RateLimiter::new(1);
        let long = "帳".repeat(1_000);
        assert!(limiter.allow_in(&long, 7));
        {
            let buckets = limiter.buckets.lock().expect("測試中的鎖不會被毒化");
            let stored = buckets.keys().next().expect("應存下一個桶");
            assert_eq!(
                stored.len(),
                MAX_KEY_BYTES / 3 * 3,
                "三位元組的字不能被切開"
            );
            assert!(long.starts_with(stored.as_str()));
        }
        assert!(
            !limiter.allow_in(&format!("{long}另一個"), 7),
            "開頭相同的長鍵併進同一個桶"
        );
    }

    #[test]
    fn zero_means_unlimited() {
        let limiter = RateLimiter::new(0);
        for _ in 0..1000 {
            assert!(limiter.allow("1"));
        }
    }

    /// The MCP allowance is a setting, so a change has to bite on the next
    /// call rather than the next restart.
    #[test]
    fn a_live_allowance_is_read_on_every_call() {
        static LIMIT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        fn read() -> u32 {
            LIMIT.load(std::sync::atomic::Ordering::Relaxed)
        }

        let limiter = RateLimiter::from_setting(read);
        assert!(limiter.allow("t"));
        assert!(!limiter.allow("t"), "超過上限就要擋下");

        LIMIT.store(0, std::sync::atomic::Ordering::Relaxed);
        assert!(limiter.allow("t"), "改成不限之後不必重新啟動就要放行");
    }
}
