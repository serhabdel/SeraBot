//! Desktop shell UI coroutine pacing after SSE wakes.

/// Poll interval while waiting for an SSE wake (milliseconds).
pub const UI_POLL_MS: u64 = 10;
/// Idle polls before sleeping again (~120ms total).
pub const UI_IDLE_POLLS: u32 = 12;
/// Pace after an SSE wake (~60fps) so transcript repaints do not spin the loop.
pub const UI_SSE_FRAME_MS: u64 = 16;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_idle_window_matches_legacy_poll_budget() {
        assert_eq!(UI_IDLE_POLLS as u64 * UI_POLL_MS, 120);
    }

    #[test]
    fn sse_wake_paces_near_sixty_fps() {
        assert_eq!(UI_SSE_FRAME_MS, 16);
    }
}
