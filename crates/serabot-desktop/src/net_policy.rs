//! Timeouts and retries for the desktop's HTTP calls, as plain functions so
//! they are unit-tested without a network.
//!
//! The client keeps its security shape (no proxy, no redirects, no cookies,
//! localhost-only plaintext); only how long a request may take and whether a
//! read is tried again live here.

use reqwest::Method;
use std::time::Duration;

/// How long a connection attempt may take. A tunnel or a cold server can need
/// several seconds, so this is generous.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default total time for a quick read.
pub const READ_TIMEOUT: Duration = Duration::from_secs(25);
/// Total time for an ordinary write (save a bot, rename, decide).
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// Total time for a call that waits on a model or an outside service: chat
/// sends, onboarding turns, connector calls (Composio and friends), uploads,
/// transcription.
pub const SLOW_TIMEOUT: Duration = Duration::from_secs(180);

/// Pauses before the second and third try of a read.
pub const GET_RETRY_DELAYS: [Duration; 2] =
    [Duration::from_millis(400), Duration::from_millis(1200)];

/// Route pieces that mark a request that legitimately takes a while.
const SLOW_PARTS: [&str; 9] = [
    "composio",
    "transcribe",
    "attachments",
    "onboarding",
    "messages",
    "test",
    "start",
    "avatar",
    "chat",
];

/// The total timeout for one request.
pub fn request_timeout(method: &Method, route: &[&str]) -> Duration {
    let slow = route.iter().any(|part| SLOW_PARTS.contains(part));
    if slow && *method != Method::GET {
        SLOW_TIMEOUT
    } else if *method == Method::GET {
        READ_TIMEOUT
    } else {
        WRITE_TIMEOUT
    }
}

/// Only plain reads are repeated: a write may have reached the server, and
/// repeating it could do it twice.
pub fn retries_allowed(method: &Method) -> bool {
    *method == Method::GET
}

/// Run `attempt` until it succeeds, fails with something that is not worth
/// repeating, or the delays run out. `sleep` is injected for tests.
pub fn with_retries<T, E>(
    delays: &[Duration],
    mut attempt: impl FnMut() -> Result<T, E>,
    retryable: impl Fn(&E) -> bool,
    mut sleep: impl FnMut(Duration),
) -> Result<T, E> {
    let mut tries = 0;
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error) => {
                let Some(delay) = delays.get(tries).copied() else {
                    return Err(error);
                };
                if !retryable(&error) {
                    return Err(error);
                }
                tries += 1;
                sleep(delay);
            }
        }
    }
}

/// How long to wait before the next automatic reconnect try: 2, 4, 8, 15, 30
/// seconds, then every 30 seconds.
pub fn reconnect_delay(attempt: u32) -> Duration {
    const STEPS: [u64; 5] = [2, 4, 8, 15, 30];
    Duration::from_secs(STEPS[(attempt as usize).min(STEPS.len() - 1)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn slow_calls_get_the_long_timeout() {
        assert_eq!(
            request_timeout(&Method::POST, &["v1", "connectors", "composio", "connect"]),
            SLOW_TIMEOUT
        );
        assert_eq!(
            request_timeout(&Method::POST, &["v1", "conversations", "c1", "messages"]),
            SLOW_TIMEOUT
        );
        assert_eq!(
            request_timeout(&Method::POST, &["v1", "audio", "transcribe"]),
            SLOW_TIMEOUT
        );
        assert_eq!(
            request_timeout(&Method::PUT, &["v1", "bots", "mia"]),
            WRITE_TIMEOUT
        );
        assert_eq!(
            request_timeout(&Method::GET, &["v1", "conversations", "c1", "messages"]),
            READ_TIMEOUT
        );
        assert!(READ_TIMEOUT > Duration::from_secs(20));
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(10));
    }

    #[test]
    fn only_reads_are_repeated() {
        assert!(retries_allowed(&Method::GET));
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(!retries_allowed(&method));
        }
    }

    #[test]
    fn a_read_is_tried_again_with_short_pauses() {
        let calls = Cell::new(0);
        let pauses = RefCell::new(Vec::new());
        let result: Result<u32, &str> = with_retries(
            &GET_RETRY_DELAYS,
            || {
                calls.set(calls.get() + 1);
                if calls.get() < 3 { Err("down") } else { Ok(7) }
            },
            |_| true,
            |delay| pauses.borrow_mut().push(delay),
        );
        assert_eq!(result, Ok(7));
        assert_eq!(calls.get(), 3);
        assert_eq!(*pauses.borrow(), GET_RETRY_DELAYS.to_vec());
    }

    #[test]
    fn retries_stop_after_the_last_delay_and_for_other_errors() {
        let calls = Cell::new(0);
        let result: Result<(), &str> = with_retries(
            &GET_RETRY_DELAYS,
            || {
                calls.set(calls.get() + 1);
                Err("down")
            },
            |_| true,
            |_| {},
        );
        assert_eq!(result, Err("down"));
        assert_eq!(calls.get(), 3);

        calls.set(0);
        let result: Result<(), &str> = with_retries(
            &GET_RETRY_DELAYS,
            || {
                calls.set(calls.get() + 1);
                Err("forbidden")
            },
            |error| *error == "down",
            |_| panic!("must not wait"),
        );
        assert_eq!(result, Err("forbidden"));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn reconnect_backs_off_and_caps() {
        let secs: Vec<u64> = (0..8).map(|n| reconnect_delay(n).as_secs()).collect();
        assert_eq!(secs, vec![2, 4, 8, 15, 30, 30, 30, 30]);
    }
}
