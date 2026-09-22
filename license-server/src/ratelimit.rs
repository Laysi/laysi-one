//! A fixed-window rate limit for console sign-in.
//!
//! Copied from the product's `laysi-server/src/ratelimit.rs` rather than
//! shared: the two workspaces share only `laysi-license-token`, and a fourth
//! shared crate for one struct costs more than the copy. The product's
//! setting-driven allowance is left out; sign-in here has a fixed one.
//!
//! Counts live in the process, so a restart clears them.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

struct Bucket {
    /// The minute the current count belongs to (Unix epoch minutes).
    window: i64,
    count: u32,
}

/// The most buckets the map holds.
///
/// The key is the submitted address, so an attacker naming a different
/// address on every attempt adds one entry per attempt. At this size the map
/// first drops buckets from earlier minutes, which changes no decision: their
/// next use would reset the count anyway. If the current minute alone fills
/// it, the bucket with the lowest count goes. Refusing the new key instead
/// would lock out every address not yet seen this minute. A flooded address
/// holds a count of one while the account being guessed holds the limit, so
/// the flood evicts its own buckets first.
///
/// Eviction can still reset a bucket, but never cheaply. To evict the account
/// being guessed at count c, a flood has to lift every other bucket to c
/// first: about c × `MAX_BUCKETS` sign-in attempts, and every one the limiter
/// admits, up to the limit per address, costs an Argon2id verification. The
/// next minute resets that bucket for free, so eviction only buys an attacker
/// more than the limit per minute at a volume that has already exhausted the
/// CPU. A bounded map that refuses nobody cannot also promise never to reset.
const MAX_BUCKETS: usize = 10_000;

/// The longest key stored, in bytes.
///
/// The key arrives in the request body, so its length is bounded only by the
/// body limit. Longer keys are cut at a character boundary; two addresses that
/// share this prefix then share a bucket, which is stricter, not looser. An
/// ASCII email address is at most 254 bytes and never reaches the cut.
const MAX_KEY_BYTES: usize = 256;

pub struct RateLimiter {
    limit_per_minute: u32,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    pub fn new(limit_per_minute: u32) -> Self {
        Self {
            limit_per_minute,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Record a call and report whether it is still within the allowance.
    pub fn allow(&self, key: &str) -> bool {
        self.allow_in(key, current_window())
    }

    fn allow_in(&self, key: &str, window: i64) -> bool {
        let mut end = key.len().min(MAX_KEY_BYTES);
        while !key.is_char_boundary(end) {
            end -= 1;
        }
        let key = &key[..end];
        // A poisoned lock must not take sign-in down with it: a possibly
        // incomplete count is better than no console.
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
        bucket.count <= self.limit_per_minute
    }
}

fn current_window() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        / 60
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::LOGIN_ATTEMPTS_PER_MINUTE;

    #[test]
    fn sign_in_is_refused_on_the_attempt_after_the_limit() {
        let limiter = RateLimiter::new(LOGIN_ATTEMPTS_PER_MINUTE);
        for n in 1..=LOGIN_ATTEMPTS_PER_MINUTE {
            assert!(limiter.allow_in("a@example.com", 7), "第 {n} 次應放行");
        }
        assert!(
            !limiter.allow_in("a@example.com", 7),
            "超過上限的那一次要擋下"
        );
        assert!(limiter.allow_in("b@example.com", 7), "另一個地址不受影響");
        assert!(limiter.allow_in("a@example.com", 8), "下一分鐘重新計算");
    }

    #[test]
    fn buckets_from_an_earlier_minute_are_swept() {
        let limiter = RateLimiter::new(1);
        for i in 0..MAX_BUCKETS {
            limiter.allow_in(&format!("舊 {i}"), 0);
        }
        assert!(limiter.allow_in("新", 1));
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
}
