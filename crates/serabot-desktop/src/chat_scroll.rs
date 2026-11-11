//! Chat message-list scroll: stick-to-bottom while streaming, reduced motion,
//! and small JS bridges the WebView runs. Thresholds are shared between Rust
//! (`onscroll`) and the bridge so stick state does not disagree.

/// Pixels from the bottom of the list that still count as "at bottom".
pub const STICK_BOTTOM_PX: f64 = 80.0;

/// Whether the scroll position is within [`STICK_BOTTOM_PX`] of the bottom.
pub fn near_bottom(scroll_height: f64, scroll_top: f64, client_height: f64) -> bool {
    (scroll_height - scroll_top - client_height) < STICK_BOTTOM_PX
}

/// A cheap fingerprint of what the transcript shows: row count, the last
/// message (id and length, so streamed tokens count) and the activity steps.
/// Two equal marks mean nothing grew, so the view must not re-pin.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscriptMark {
    pub count: usize,
    pub tail: Option<(String, usize)>,
    pub steps: usize,
    pub step_tail: Option<(String, usize)>,
}

/// Whether the open conversation is a different one from the one the view last
/// set itself up for. The view re-runs its effects on every session write, so
/// per-conversation resets must be gated on this, not run unconditionally.
pub fn conversation_switched(current: &str, seen: &str) -> bool {
    current != seen
}

/// Whether the chat view should call the scroll pin bridge after a transcript or
/// activity change, and whether to force stick-to-bottom.
///
/// Only the owner's own send or a freshly opened conversation forces a jump.
/// Streaming growth pins softly: the bridge follows only while it still knows
/// the reader is at the bottom, so scrolling up is never overridden by a stale
/// Rust-side flag.
pub fn transcript_pin_action(
    previous_count: usize,
    count: usize,
    own_send: bool,
    streaming_growth: bool,
) -> Option<bool> {
    let force_pin = own_send || previous_count == 0;
    if force_pin || count != previous_count || streaming_growth {
        Some(force_pin)
    } else {
        None
    }
}

/// Pin or refresh the message list when transcript rows grow (streaming tokens).
pub fn pin_messages_js(force: bool) -> String {
    format!(
        "window.__serabotPinMessages?.({});",
        if force { "true" } else { "false" }
    )
}

/// Scroll the list to the newest content; smooth only when motion is not reduced.
pub const SCROLL_TO_BOTTOM_JS: &str = r#"
window.__serabotPinMessages?.(true);
const el = document.getElementById('messages');
if (el) {
  const reduced =
    window.matchMedia('(prefers-reduced-motion: reduce)').matches ||
    document.querySelector('.root')?.dataset.motion === 'reduce';
  const top = el.scrollHeight - el.clientHeight;
  el.scrollTo({ top, behavior: reduced ? 'auto' : 'smooth' });
}
"#;

/// Keeps the message list pinned to the bottom while content grows (streaming
/// tokens, activity rows) without fighting a scroll-up. `data-stick` tracks
/// whether the owner is near the bottom; ResizeObserver pins only then.
/// While stuck, scroll follows growth with a short eased chase unless reduced
/// motion is on (then it snaps).
pub const CHAT_SCROLL_BRIDGE_JS: &str = r#"
(function () {
  if (window.__serabotChatScroll) return;
  window.__serabotChatScroll = true;
  const BOTTOM = 80;
  const CHASE = 0.38;
  let chaseId = 0;

  const el = () => document.getElementById('messages');

  const reducedMotion = () =>
    window.matchMedia('(prefers-reduced-motion: reduce)').matches ||
    document.querySelector('.root')?.dataset.motion === 'reduce';

  const destTop = (box) => Math.max(0, box.scrollHeight - box.clientHeight);

  const cancelChase = () => {
    if (chaseId) {
      cancelAnimationFrame(chaseId);
      chaseId = 0;
    }
  };

  const chaseToBottom = () => {
    const box = el();
    if (!box || box.dataset.stick !== '1') {
      cancelChase();
      return;
    }
    const target = destTop(box);
    if (reducedMotion()) {
      box.scrollTop = target;
      return;
    }
    const cur = box.scrollTop;
    const delta = target - cur;
    if (Math.abs(delta) < 0.75) {
      box.scrollTop = target;
      cancelChase();
      return;
    }
    box.scrollTop = cur + delta * CHASE;
    cancelChase();
    chaseId = requestAnimationFrame(chaseToBottom);
  };

  const pinIfStuck = () => {
    const box = el();
    if (!box || box.dataset.stick !== '1') return;
    if (reducedMotion()) {
      cancelChase();
      box.scrollTop = destTop(box);
      return;
    }
    if (!chaseId) chaseToBottom();
  };

  const updateStick = (box) => {
    if (!box) return;
    const near = box.scrollHeight - box.scrollTop - box.clientHeight < BOTTOM;
    const stuck = near ? '1' : '0';
    if (box.dataset.stick !== stuck) {
      box.dataset.stick = stuck;
      if (stuck === '0') cancelChase();
    }
  };

  window.__serabotPinMessages = (force) => {
    const box = el();
    if (!box) return;
    if (force) box.dataset.stick = '1';
    pinIfStuck();
  };

  document.addEventListener(
    'scroll',
    (event) => {
      const target = event.target;
      if (target && target.id === 'messages') updateStick(target);
    },
    { capture: true, passive: true }
  );

  const observer = new ResizeObserver(() => pinIfStuck());
  const hook = () => {
    const box = el();
    if (!box || box.__serabotScrollHook) return;
    box.__serabotScrollHook = true;
    observer.observe(box);
    updateStick(box);
    pinIfStuck();
  };
  hook();
  new MutationObserver(hook).observe(document.body, { childList: true, subtree: true });
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_bottom_uses_shared_threshold() {
        let client = 400.0;
        let height = 1200.0;
        assert!(!near_bottom(height, 0.0, client));
        assert!(near_bottom(height, height - client - 79.0, client));
        assert!(!near_bottom(height, height - client - 81.0, client));
        assert!(near_bottom(height, height - client, client));
    }

    #[test]
    fn pin_script_matches_bridge_entrypoint() {
        assert_eq!(
            pin_messages_js(true),
            "window.__serabotPinMessages?.(true);"
        );
        assert_eq!(
            pin_messages_js(false),
            "window.__serabotPinMessages?.(false);"
        );
    }

    #[test]
    fn bridge_installs_once_and_exposes_pin() {
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("window.__serabotChatScroll"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("window.__serabotPinMessages"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("reducedMotion"));
    }

    #[test]
    fn bridge_bottom_threshold_matches_rust() {
        assert!(
            CHAT_SCROLL_BRIDGE_JS.contains("const BOTTOM = 80"),
            "JS stick threshold must match STICK_BOTTOM_PX"
        );
        assert_eq!(STICK_BOTTOM_PX, 80.0);
    }

    #[test]
    fn scroll_to_bottom_snaps_when_reduced() {
        assert!(SCROLL_TO_BOTTOM_JS.contains("behavior: reduced ? 'auto' : 'smooth'"));
        assert!(SCROLL_TO_BOTTOM_JS.contains("dataset.motion === 'reduce'"));
    }

    #[test]
    fn reduced_motion_snaps_scroll_top_in_bridge() {
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("if (reducedMotion())"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("box.scrollTop = target"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("box.scrollTop = destTop(box)"));
    }

    #[test]
    fn near_bottom_after_list_grows_while_pinned() {
        let client = 400.0;
        let top = 800.0;
        assert!(near_bottom(1200.0, top, client));
        assert!(near_bottom(2400.0, top + 1200.0, client));
        assert!(!near_bottom(2400.0, top, client));
    }

    #[test]
    fn streaming_growth_pins_softly_and_never_forces() {
        // The bridge decides from its own stick state; Rust must not force it
        // from a flag that can be stale after a scroll-up.
        assert_eq!(transcript_pin_action(3, 3, false, true), Some(false));
    }

    #[test]
    fn transcript_pin_skips_when_nothing_grew() {
        assert_eq!(transcript_pin_action(3, 3, false, false), None);
    }

    #[test]
    fn transcript_pin_on_new_row_is_soft() {
        assert_eq!(transcript_pin_action(3, 4, false, false), Some(false));
    }

    #[test]
    fn own_send_and_first_load_force_a_jump() {
        assert_eq!(transcript_pin_action(3, 4, true, false), Some(true));
        assert_eq!(transcript_pin_action(0, 5, false, false), Some(true));
    }

    #[test]
    fn an_unchanged_mark_is_not_growth() {
        // Regression: any session write used to count as "streaming growth".
        let mark = TranscriptMark {
            count: 4,
            tail: Some(("m4".into(), 120)),
            steps: 1,
            step_tail: Some(("c1".into(), 8)),
        };
        assert_eq!(mark, mark.clone());
        let mut grown = mark.clone();
        grown.tail = Some(("m4".into(), 125));
        assert_ne!(mark, grown, "a streamed token changes the mark");
        let mut stepped = mark.clone();
        stepped.steps = 2;
        assert_ne!(mark, stepped);
    }

    #[test]
    fn switch_is_detected_only_on_a_different_conversation() {
        assert!(!conversation_switched("dir:main", "dir:main"));
        assert!(conversation_switched("dir:legal", "dir:main"));
    }
}
