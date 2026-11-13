//! Chat message-list scroll: stick-to-bottom while streaming, reduced motion,
//! and small JS bridges the WebView runs. The page owns the follow state and
//! reports changes; Rust does not recompute it from scroll positions.

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

/// Jump to the newest message and resume following. Instant on purpose: a
/// smooth scroll toward a moving target (a reply still streaming) stops short
/// and fights the follow logic, which was the "unstable" jump button.
pub const SCROLL_TO_BOTTOM_JS: &str = "window.__serabotJumpToLatest?.();";

/// Starts the sink that receives follow-state changes from the page. Follow
/// state is owned by the bridge below and reported only when it changes, so
/// Rust never derives it from scroll positions (which arrive late and, while a
/// reply streams, read as "far from the bottom" and flicker the pill).
pub const FOLLOW_SINK_JS: &str = r#"
window.__serabotFollowSink = (value) => dioxus.send(value);
window.__serabotResync?.();
"#;

/// Whether a scroll that moved the list up should be read as the reader
/// scrolling away. Programmatic pins only ever move down, and a list that
/// shrank (a panel collapsed) clamps `scrollTop` without any reader input, so
/// neither counts. Mirrors the bridge below.
pub fn is_reader_scroll_up(previous_top: f64, top: f64, previous_height: f64, height: f64) -> bool {
    top < previous_top - 1.0 && height >= previous_height
}

/// Keeps the message list following the newest content while the reader has
/// not scrolled away, and tells Rust only when that state changes.
///
/// - Follow turns OFF only on reader intent: wheel up, touch drag, a key that
///   scrolls up, or a scrollbar drag that moves the list up. Content growth
///   never turns it off.
/// - Follow turns ON when a 1px sentinel at the end of the list is visible
///   (IntersectionObserver, 80px margin) or on an explicit jump.
/// - While following, growth (ResizeObserver on the rows, MutationObserver on
///   the list) snaps to the bottom once per frame. No easing chase to lag
///   behind a fast stream.
pub const CHAT_SCROLL_BRIDGE_JS: &str = r#"
(function () {
  if (window.__serabotChatScroll) return;
  window.__serabotChatScroll = true;
  const BOTTOM = 80;
  let follow = true;
  let reported = null;
  let lastTop = 0;
  let lastHeight = 0;
  let raf = 0;
  const seen = new WeakSet();
  // Where the reader was in each chat they scrolled away from (kept while the
  // app runs). `pending` restores one after the transcript has loaded.
  const saved = {};
  let pending = null;

  const box = () => document.getElementById('messages');
  const convOf = (b) => (b && b.dataset && b.dataset.conv) || '';

  // Put a remembered position back once the page is tall enough; give up
  // after a short while so an empty or short chat never waits.
  const tryRestore = () => {
    const b = box();
    if (!b || !pending) return;
    if (convOf(b) !== pending.conv) {
      pending = null;
      return;
    }
    pending.tries += 1;
    const room = b.scrollHeight - b.clientHeight;
    if (room >= pending.top || pending.tries > 40) {
      b.scrollTop = Math.min(pending.top, Math.max(0, room));
      lastTop = b.scrollTop;
      lastHeight = b.scrollHeight;
      pending = null;
    }
  };

  const report = () => {
    if (reported === follow) return;
    reported = follow;
    const b = box();
    if (b) b.dataset.stick = follow ? '1' : '0';
    if (window.__serabotFollowSink) window.__serabotFollowSink(follow);
  };

  const snap = () => {
    const b = box();
    if (!b) return;
    b.scrollTop = b.scrollHeight;
    lastTop = b.scrollTop;
    lastHeight = b.scrollHeight;
  };

  const setFollow = (value) => {
    follow = value;
    report();
  };

  const schedule = () => {
    if (raf) return;
    raf = requestAnimationFrame(() => {
      raf = 0;
      if (pending) {
        tryRestore();
        return;
      }
      if (follow) snap();
    });
  };

  window.__serabotResync = () => {
    reported = null;
    report();
  };

  window.__serabotPinMessages = (force) => {
    if (pending) {
      schedule();
      return;
    }
    if (force) {
      follow = true;
      snap();
      report();
    } else {
      schedule();
    }
  };

  window.__serabotJumpToLatest = () => {
    follow = true;
    snap();
    report();
  };

  const away = () => setFollow(false);

  const hook = () => {
    const b = box();
    if (!b) return;
    if (!b.__serabotHooked) {
      b.__serabotHooked = true;
      lastTop = b.scrollTop;
      lastHeight = b.scrollHeight;
      // A chat the reader left scrolled up comes back at the same place.
      const memory = saved[convOf(b)];
      if (memory && memory.stick === false) {
        pending = { conv: convOf(b), top: memory.top, tries: 0 };
        follow = false;
        report();
      }
      b.addEventListener('wheel', (e) => { if (e.deltaY < 0) away(); }, { passive: true });
      b.addEventListener('touchmove', away, { passive: true });
      b.addEventListener('keydown', (e) => {
        if (e.key === 'PageUp' || e.key === 'ArrowUp' || e.key === 'Home') away();
      });
      b.addEventListener('scroll', () => {
        const top = b.scrollTop;
        const height = b.scrollHeight;
        if (top < lastTop - 1 && height >= lastHeight) setFollow(false);
        lastTop = top;
        lastHeight = height;
        if (!pending) saved[convOf(b)] = { top: top, stick: follow };
      }, { passive: true });
      const io = new IntersectionObserver((entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) setFollow(true);
        }
      }, { root: b, rootMargin: '0px 0px ' + BOTTOM + 'px 0px' });
      b.__serabotIo = io;
    }
    const sentinel = document.getElementById('messages-end');
    if (sentinel && !seen.has(sentinel)) {
      seen.add(sentinel);
      b.__serabotIo.observe(sentinel);
    }
    for (const child of b.children) {
      if (!seen.has(child)) {
        seen.add(child);
        ro.observe(child);
      }
    }
  };

  const ro = new ResizeObserver(() => schedule());
  const mo = new MutationObserver(() => {
    hook();
    schedule();
  });
  const attach = () => {
    const b = box();
    if (!b || b.__serabotWatched) return;
    b.__serabotWatched = true;
    mo.observe(b, { childList: true, subtree: true, characterData: true });
    ro.observe(b);
    hook();
    schedule();
  };
  attach();
  new MutationObserver(attach).observe(document.body, { childList: true, subtree: true });
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
    fn bridge_installs_once_and_exposes_its_entrypoints() {
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("window.__serabotChatScroll"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("window.__serabotPinMessages"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("window.__serabotJumpToLatest"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("window.__serabotResync"));
        assert!(SCROLL_TO_BOTTOM_JS.contains("__serabotJumpToLatest"));
        assert!(FOLLOW_SINK_JS.contains("dioxus.send"));
    }

    #[test]
    fn bridge_uses_a_sentinel_and_the_shared_threshold() {
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("IntersectionObserver"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("messages-end"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("const BOTTOM = 80"));
        assert_eq!(STICK_BOTTOM_PX, 80.0);
    }

    #[test]
    fn bridge_follows_growth_without_an_easing_chase() {
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("new ResizeObserver"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("new MutationObserver"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("b.scrollTop = b.scrollHeight"));
        assert!(!CHAT_SCROLL_BRIDGE_JS.contains("CHASE"));
        // The jump is instant: a smooth scroll to a moving target stops short.
        assert!(!SCROLL_TO_BOTTOM_JS.contains("smooth"));
    }

    #[test]
    fn bridge_disengages_only_on_reader_intent() {
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("'wheel'"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("'touchmove'"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("top < lastTop - 1 && height >= lastHeight"));
    }

    #[test]
    fn reader_scroll_up_is_told_apart_from_growth_and_shrink() {
        // The reader moved up in a list of the same size.
        assert!(is_reader_scroll_up(900.0, 600.0, 1500.0, 1500.0));
        // A pin moves down.
        assert!(!is_reader_scroll_up(600.0, 900.0, 1500.0, 1500.0));
        // Streaming grew the list; the position did not move up.
        assert!(!is_reader_scroll_up(900.0, 900.0, 1500.0, 1700.0));
        // A panel collapsed: the browser clamped scrollTop, the reader did nothing.
        assert!(!is_reader_scroll_up(900.0, 700.0, 1500.0, 1300.0));
        // One-pixel rounding is not a scroll.
        assert!(!is_reader_scroll_up(900.0, 899.5, 1500.0, 1500.0));
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
