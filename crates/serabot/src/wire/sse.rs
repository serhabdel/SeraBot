//! A tolerant server-sent-events reader for a finished response body.
//!
//! Handles `\n` and `\r\n` line ends, comment lines, multi-line `data:` fields
//! and a final event with no trailing blank line. Bodies are read whole (the
//! caller caps their size), so this never sees a partial event.

/// One event: its optional `event:` name and its joined `data:` text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Every event in `body`, in order. Events with no data are skipped.
pub fn events(body: &[u8]) -> Vec<SseEvent> {
    let text = String::from_utf8_lossy(body);
    let mut out = Vec::new();
    let mut event: Option<String> = None;
    let mut data: Vec<&str> = Vec::new();
    let flush = |event: &mut Option<String>, data: &mut Vec<&str>, out: &mut Vec<SseEvent>| {
        if !data.is_empty() {
            out.push(SseEvent {
                event: event.take(),
                data: data.join("\n"),
            });
        } else {
            *event = None;
        }
        data.clear();
    };
    for line in text.lines() {
        if line.is_empty() {
            flush(&mut event, &mut data, &mut out);
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => event = Some(value.to_owned()),
            "data" => data.push(value),
            _ => {}
        }
    }
    flush(&mut event, &mut data, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_named_events_comments_and_crlf() {
        let body = b": keepalive\r\nevent: a\r\ndata: {\"x\":1}\r\n\r\nevent: b\r\ndata: one\r\ndata: two\r\n\r\ndata: last";
        let all = events(body);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].event.as_deref(), Some("a"));
        assert_eq!(all[0].data, "{\"x\":1}");
        assert_eq!(all[1].data, "one\ntwo");
        assert_eq!(all[2].event, None);
        assert_eq!(all[2].data, "last");
    }

    #[test]
    fn an_event_with_no_data_is_dropped() {
        assert!(events(b"event: ping\n\n").is_empty());
        assert!(events(b"").is_empty());
    }
}
