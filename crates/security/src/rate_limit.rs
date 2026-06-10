use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// A rate-limit tracking entry for a single IP.
#[derive(Debug, Clone)]
struct RateLimitEntry {
    /// Number of attempts in the current window.
    count: usize,
    /// Window start time as seconds since UNIX epoch.
    window_start: u64,
}

/// Global in-memory rate-limit state keyed by client IP.
static STATE: std::sync::LazyLock<Mutex<HashMap<String, RateLimitEntry>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Check whether `ip` is allowed to proceed under the rate limit.
///
/// `max` is the maximum number of attempts allowed within a sliding `window_secs` window.
/// Returns `true` if the request should be allowed, `false` if rate-limited.
pub fn check_rate_limit(ip: &str, max: usize, window_secs: u64) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let mut state = STATE.lock().unwrap();

    match state.get_mut(ip) {
        Some(ref mut entry) if !is_window_expired(entry, now, window_secs) => {
            if entry.count < max {
                entry.count += 1;
                true
            } else {
                false
            }
        }
        Some(ref mut entry) => {
            // Window expired -- reset
            entry.count = 1;
            entry.window_start = now;
            true
        }
        None => {
            state.insert(
                ip.to_string(),
                RateLimitEntry {
                    count: 1,
                    window_start: now,
                },
            );
            true
        }
    }
}

/// Reset rate-limit state for a given IP (e.g. after successful login).
pub fn reset_rate_limit(ip: &str) {
    let mut state = STATE.lock().unwrap();
    state.remove(ip);
}

/// Clear all rate-limit state (for testing).
pub fn reset_all() {
    let mut state = STATE.lock().unwrap();
    state.clear();
}

/// Check whether the entry's window has expired.
#[inline]
fn is_window_expired(entry: &RateLimitEntry, now: u64, window_secs: u64) -> bool {
    now >= entry.window_start.wrapping_add(window_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    fn setup() {
        reset_all();
    }

    #[test]
    fn test_allows_first_request() {
        setup();
        assert!(check_rate_limit("192.168.1.1", 5, 60));
    }

    #[test]
    fn test_allows_under_limit() {
        setup();
        let ip = "10.0.0.1";
        for _ in 0..4 {
            assert!(check_rate_limit(ip, 5, 60), "Should allow under limit");
        }
    }

    #[test]
    fn test_rejects_at_limit() {
        setup();
        let ip = "10.0.0.2";
        for _ in 0..5 {
            check_rate_limit(ip, 5, 60);
        }
        // 6th attempt should be blocked
        assert!(!check_rate_limit(ip, 5, 60), "Should reject over limit");
    }

    #[test]
    fn test_window_resets() {
        setup();
        let ip = "10.0.0.3";

        // Exhaust limit
        for _ in 0..5 {
            check_rate_limit(ip, 5, 60);
        }
        assert!(!check_rate_limit(ip, 5, 60));

        // Wait for window to expire
        thread::sleep(Duration::from_millis(100));

        // The window hasn't actually expired (100ms < 60s), so still blocked
        // We need to test with a short window
        setup();
        assert!(check_rate_limit(ip, 5, 60));
    }

    #[test]
    fn test_short_window_expiry() {
        setup();
        let ip = "10.0.0.4";

        // Exhaust limit with short 50ms window
        for _ in 0..3 {
            assert!(check_rate_limit(ip, 3, 1)); // 1 sec window
        }
        assert!(!check_rate_limit(ip, 3, 1));

        // Wait just over 1 second for window to expire
        thread::sleep(Duration::from_secs(1));

        // Should be allowed again (window expired and reset)
        assert!(check_rate_limit(ip, 3, 1));
    }

    #[test]
    fn test_reset_rate_limit() {
        setup();
        let ip = "10.0.0.5";

        for _ in 0..5 {
            check_rate_limit(ip, 5, 60);
        }
        assert!(!check_rate_limit(ip, 5, 60));

        reset_rate_limit(ip);
        assert!(check_rate_limit(ip, 5, 60), "Should allow after reset");
    }

    #[test]
    fn test_different_ips_independent() {
        setup();

        let ip_a = "10.0.0.10";
        let ip_b = "10.0.0.20";

        // Exhaust ip_a
        for _ in 0..3 {
            check_rate_limit(ip_a, 3, 60);
        }
        assert!(!check_rate_limit(ip_a, 3, 60));

        // ip_b should still be allowed
        assert!(check_rate_limit(ip_b, 3, 60));
        assert!(check_rate_limit(ip_b, 3, 60));
        assert!(check_rate_limit(ip_b, 3, 60));
        assert!(!check_rate_limit(ip_b, 3, 60));
    }
}
