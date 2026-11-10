//! Chat message-list scroll: stick-to-bottom while streaming, reduced motion,
//! and small JS bridges the WebView runs. Thresholds are shared between Rust
//! (`onscroll`) and the bridge so stick state does not disagree.

/// Pixels from the bottom of the list that still count as "at bottom".
pub const STICK_BOTTOM_PX: f64 = 80.0;

/// Whether the scroll position is within [`STICK_BOTTOM_PX`] of the bottom.
pub fn near_bottom(scroll_height: f64, scroll_top: f64, client_height: f64) -> bool {
    (scroll_height - scroll_top - client_height) < STICK_BOTTOM_PX
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
        assert_eq!(pin_messages_js(true), "window.__serabotPinMessages?.(true);");
        assert_eq!(pin_messages_js(false), "window.__serabotPinMessages?.(false);");
    }

    #[test]
    fn bridge_installs_once_and_exposes_pin() {
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("window.__serabotChatScroll"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("window.__serabotPinMessages"));
        assert!(CHAT_SCROLL_BRIDGE_JS.contains("reducedMotion"));
    }
}
