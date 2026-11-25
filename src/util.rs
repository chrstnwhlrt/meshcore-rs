//! Small helpers shared by the modules.

use std::future::{Future, poll_fn};
use std::hash::{BuildHasher, Hasher};
use std::pin::pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Poll;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The current time as Unix seconds, as the protocol's 32-bit timestamps use it.
pub(crate) fn unix_now() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX))
}

/// The current time as Unix seconds (64 bit), for receive timestamps.
pub(crate) fn unix_now_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A random 32-bit value for request tags. Not for cryptographic use.
pub(crate) fn random_u32() -> u32 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    let value = hasher.finish();
    // Never 0, like meshcore_py's random.randint(1, 0xFFFFFFFF).
    let folded = (value ^ (value >> 32)) as u32;
    if folded == 0 { 1 } else { folded }
}

/// The longest start of `text` that fits in `max` bytes without splitting a
/// character (the firmware keeps name fields to 31 bytes).
pub(crate) fn cut_at_char_boundary(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The instant `timeout` from now. A timeout too long to add, as
/// `Duration::MAX` for "no limit", gives one 30 years ahead (as Tokio's own
/// timeouts do) instead of a panic.
pub(crate) fn deadline_after(timeout: Duration) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    now.checked_add(timeout)
        .unwrap_or_else(|| now + Duration::from_secs(30 * 365 * 24 * 60 * 60))
}

/// What `future` gives, or `None` once `end` is done (what is ready by then
/// counts).
pub(crate) async fn until<T>(
    end: impl Future + Send,
    future: impl Future<Output = T> + Send,
) -> Option<T> {
    let mut end = pin!(end);
    let mut future = pin!(future);
    poll_fn(|cx| {
        if let Poll::Ready(value) = future.as_mut().poll(cx) {
            return Poll::Ready(Some(value));
        }
        end.as_mut().poll(cx).map(|_| None)
    })
    .await
}

/// How long to wait for a remote answer, given the device's suggested timeout
/// in milliseconds: meshcore_py waits `suggested_timeout / 800` seconds, i.e.
/// a quarter longer than suggested.
pub(crate) fn suggested_wait(suggested_ms: u32) -> Duration {
    Duration::from_micros(u64::from(suggested_ms) * 1250)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn until_gives_what_is_ready_first() {
        use std::future::{pending, ready};
        assert_eq!(until(pending::<()>(), ready(1)).await, Some(1));
        assert_eq!(until(ready(()), ready(1)).await, Some(1));
        assert_eq!(until(ready(()), pending::<u8>()).await, None);
    }

    #[test]
    fn suggested_wait_is_a_quarter_longer() {
        assert_eq!(suggested_wait(4000), Duration::from_secs(5));
        assert_eq!(suggested_wait(0), Duration::ZERO);
    }

    #[test]
    fn endless_timeouts_do_not_overflow() {
        let far = deadline_after(Duration::MAX);
        assert!(far > tokio::time::Instant::now() + Duration::from_secs(365 * 24 * 60 * 60));
        assert!(deadline_after(Duration::ZERO) <= tokio::time::Instant::now());
    }

    #[test]
    fn cuts_text_at_a_char_boundary() {
        assert_eq!(cut_at_char_boundary("abc", 31), "abc");
        assert_eq!(cut_at_char_boundary("abcd", 2), "ab");
        // "é" is two bytes: no half of it
        assert_eq!(cut_at_char_boundary("aé", 2), "a");
        assert_eq!(cut_at_char_boundary("é", 0), "");
    }

    #[test]
    fn random_values_differ_and_are_not_zero() {
        let a = random_u32();
        let b = random_u32();
        assert_ne!(a, 0);
        assert_ne!(a, b);
    }
}
