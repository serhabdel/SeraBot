//! The chat feed's windowing (v0.50 C1): only the rows near the viewport are
//! drawn, the rest stand in as two spacers of estimated height. The page
//! reports the first visible row; Rust keeps it as a message id so older pages
//! loading in front never move the window.

use super::*;

/// Rows that fill a viewport, as an upper bound.
pub const VIEWPORT_ROWS: usize = 24;
/// Extra rows drawn above and below the viewport.
pub const OVERSCAN_ROWS: usize = 16;
/// Height a row not on screen is assumed to have (px). Both the spacers and
/// the page's scroll maths use it.
pub const EST_ROW_PX: usize = 96;
/// The page reports a new first row only after it moved this many rows, so a
/// scroll does not re-render the feed on every wheel tick.
pub const REPORT_STEP_ROWS: usize = 6;

/// One prepared transcript row, with what it is drawn with this time.
pub(super) struct RowView {
    pub(super) row: Rc<MsgView>,
    pub(super) class: String,
    pub(super) bot_meta: Option<(String, bool)>,
    pub(super) media: Vec<serabot_desktop::LibraryEntry>,
}

/// The slice of the transcript that is drawn: `start..end` of `total` rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedWindow {
    pub start: usize,
    pub end: usize,
    pub total: usize,
}

impl FeedWindow {
    #[cfg(test)]
    pub fn contains(&self, index: usize) -> bool {
        index >= self.start && index < self.end
    }

    /// Height of the stand-in for the rows above the window.
    pub fn top_spacer_px(&self) -> usize {
        self.start * EST_ROW_PX
    }

    /// Height of the stand-in for the rows below the window.
    pub fn bottom_spacer_px(&self) -> usize {
        (self.total - self.end) * EST_ROW_PX
    }
}

/// The rows to draw. While the reader follows the newest message the window is
/// the tail; otherwise it is centred on the first visible row (`anchor`).
pub fn feed_window(total: usize, anchor: Option<usize>, following: bool) -> FeedWindow {
    let size = VIEWPORT_ROWS + 2 * OVERSCAN_ROWS;
    if total <= size {
        return FeedWindow {
            start: 0,
            end: total,
            total,
        };
    }
    let start = match anchor {
        Some(index) if !following => index
            .saturating_sub(OVERSCAN_ROWS)
            .min(total.saturating_sub(size)),
        _ => total - size,
    };
    FeedWindow {
        start,
        end: (start + size).min(total),
        total,
    }
}

/// Where the remembered first-visible message is now. `None` when it is gone
/// (another chat), which falls back to the tail.
pub fn anchor_index<'a>(ids: impl IntoIterator<Item = &'a str>, anchor: &str) -> Option<usize> {
    ids.into_iter().position(|id| id == anchor)
}

/// Whether a reported first row differs enough from the last one to redraw.
pub fn moved_enough(last: Option<usize>, reported: usize) -> bool {
    last.is_none_or(|last| last.abs_diff(reported) >= REPORT_STEP_ROWS)
}

/// Reports the first visible row to Rust. With the window's top spacer
/// (`#feed-spacer-top`, `data-start`) in view the row is estimated from the
/// scroll offset; otherwise it is the first `.msg` whose bottom is on screen.
/// Reports only after a move of `REPORT_STEP_ROWS`.
pub fn feed_window_js() -> String {
    format!(
        r#"
(function () {{
  if (window.__serabotFeedWindow) return;
  window.__serabotFeedWindow = true;
  const EST = {est};
  const STEP = {step};
  let last = -1;
  let raf = 0;
  const compute = () => {{
    raf = 0;
    const b = document.getElementById('messages');
    if (!b) return;
    const sp = document.getElementById('feed-spacer-top');
    const start = sp ? Number(sp.dataset.start) || 0 : 0;
    const spacer = sp ? sp.offsetHeight : 0;
    let ix;
    if (b.scrollTop < spacer) {{
      ix = Math.floor(b.scrollTop / EST);
    }} else {{
      ix = start;
      const top = b.getBoundingClientRect().top;
      const rows = b.querySelectorAll(':scope > .msg');
      for (let k = 0; k < rows.length; k++) {{
        if (rows[k].getBoundingClientRect().bottom > top) {{ ix = start + k; break; }}
      }}
    }}
    if (last < 0 || Math.abs(ix - last) >= STEP) {{
      last = ix;
      if (window.__serabotFeedSink) window.__serabotFeedSink(ix);
    }}
  }};
  const schedule = () => {{ if (!raf) raf = requestAnimationFrame(compute); }};
  const hook = () => {{
    const b = document.getElementById('messages');
    if (!b) return;
    if (!b.__serabotFeedHooked) {{
      b.__serabotFeedHooked = true;
      last = -1;
      b.addEventListener('scroll', schedule, {{ passive: true }});
    }}
  }};
  new MutationObserver(hook).observe(document.body, {{ childList: true, subtree: true }});
  hook();
}})();
"#,
        est = EST_ROW_PX,
        step = REPORT_STEP_ROWS,
    )
}

/// Starts the sink that receives the first visible row from the page.
pub const FEED_SINK_JS: &str = r#"
window.__serabotFeedSink = (value) => dioxus.send(value);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_transcript_is_drawn_whole() {
        let w = feed_window(30, None, true);
        assert_eq!((w.start, w.end), (0, 30));
        assert_eq!(w.top_spacer_px() + w.bottom_spacer_px(), 0);
    }

    #[test]
    fn five_thousand_messages_keep_node_count_bounded() {
        let total = 5_000;
        let cap = VIEWPORT_ROWS + 2 * OVERSCAN_ROWS;
        // Following, and anchored at the top, the middle, the end and out of range.
        for (anchor, following) in [
            (None, true),
            (Some(0), false),
            (Some(2_500), false),
            (Some(4_999), false),
            (Some(9_999), false),
        ] {
            let w = feed_window(total, anchor, following);
            assert!(
                w.end - w.start <= cap,
                "{anchor:?} drew {} rows",
                w.end - w.start
            );
            assert!(w.end <= total);
            // Spacers stand in for the rest, so the scroll height stays whole.
            assert_eq!(
                w.top_spacer_px() + w.bottom_spacer_px() + (w.end - w.start) * EST_ROW_PX,
                total * EST_ROW_PX
            );
        }
        let tail = feed_window(total, None, true);
        assert_eq!(tail.end, total);
        assert_eq!(tail.bottom_spacer_px(), 0);
    }

    #[test]
    fn anchor_stays_on_its_message_when_older_pages_load() {
        let page: Vec<String> = (100..400).map(|i| format!("m{i}")).collect();
        let first_visible = "m250";
        let before = anchor_index(page.iter().map(String::as_str), first_visible).unwrap();
        let w_before = feed_window(page.len(), Some(before), false);
        assert!(w_before.contains(before));

        // 100 older messages land in front.
        let mut grown: Vec<String> = (0..100).map(|i| format!("m{i}")).collect();
        grown.extend(page.iter().cloned());
        let after = anchor_index(grown.iter().map(String::as_str), first_visible).unwrap();
        let w_after = feed_window(grown.len(), Some(after), false);
        assert_eq!(after, before + 100);
        assert!(w_after.contains(after));
        // The same messages are drawn: the window moved with the ids.
        assert_eq!(grown[w_after.start], page[w_before.start]);
        assert_eq!(grown[w_after.end - 1], page[w_before.end - 1]);
    }

    #[test]
    fn unknown_anchor_is_none() {
        assert_eq!(anchor_index(["a", "b"], "z"), None);
    }

    #[test]
    fn reports_need_a_real_move() {
        assert!(moved_enough(None, 10));
        assert!(!moved_enough(Some(10), 12));
        assert!(moved_enough(Some(10), 10 + REPORT_STEP_ROWS));
        assert!(moved_enough(Some(30), 10));
    }

    #[test]
    fn bridge_embeds_the_shared_constants() {
        let js = feed_window_js();
        assert!(js.contains(&format!("const EST = {EST_ROW_PX};")));
        assert!(js.contains(&format!("const STEP = {REPORT_STEP_ROWS};")));
    }
}
