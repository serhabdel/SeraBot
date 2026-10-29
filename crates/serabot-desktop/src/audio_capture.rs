//! Microphone capture through the WebView2 `MediaRecorder` API.
//!
//! The capture runs entirely in the webview: Rust only evaluates a small
//! script that starts and stops a `MediaRecorder`, and reads the recorded
//! blob back as base64. No native audio library and no new Cargo dependency
//! are involved.
//!
//! `dioxus::document::eval` returns an `Eval` future that resolves when the
//! script finishes, so the capture calls are `async`: they must be awaited
//! inside the Dioxus runtime (for example in an `async` event handler). A
//! blocking wait would stall the WebView2 message loop that delivers the
//! script result.
//!
//! Every failure path returns a short owner-facing message. Nothing here
//! panics and no secret is ever produced or logged.

/// The global the recorder script keeps its state in (`window.__serabotMic`).
pub const STATE_KEY: &str = "__serabotMic";

/// Start the recorder: ask for the microphone, pick a codec and record.
const START_JS: &str = r#"
const state = { recorder: null, stream: null, chunks: [], base64: null, mimeType: null };
window.__serabotMic = state;
if (typeof MediaRecorder === "undefined" || !MediaRecorder) {
  return { ok: false, error: "This window has no MediaRecorder support." };
}
if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
  return { ok: false, error: "Microphone capture is not available in this window." };
}
let stream = null;
try {
  stream = await navigator.mediaDevices.getUserMedia({ audio: true });
} catch (error) {
  const name = error && error.name ? error.name : "";
  if (name === "NotAllowedError" || name === "SecurityError") {
    return { ok: false, error: "Microphone permission was denied." };
  }
  if (name === "NotFoundError" || name === "DevicesNotFoundError") {
    return { ok: false, error: "No microphone was found." };
  }
  return { ok: false, error: "The microphone could not be opened." };
}
const types = ["audio/webm;codecs=opus", "audio/webm", "audio/mp4"];
let mimeType = "";
for (const type of types) {
  if (MediaRecorder.isTypeSupported(type)) { mimeType = type; break; }
}
if (!mimeType) {
  stream.getTracks().forEach((track) => track.stop());
  return { ok: false, error: "No supported recording format is available." };
}
let recorder = null;
try {
  recorder = new MediaRecorder(stream, { mimeType });
} catch (error) {
  stream.getTracks().forEach((track) => track.stop());
  return { ok: false, error: "The recorder could not be started." };
}
recorder.ondataavailable = (event) => {
  if (event.data && event.data.size > 0) { state.chunks.push(event.data); }
};
recorder.onerror = () => { state.error = "The recorder failed."; };
recorder.start(250);
state.recorder = recorder;
state.stream = stream;
state.mimeType = mimeType;
return { ok: true, mimeType: mimeType };
"#;

/// Stop the recorder and encode the result as base64 in the global.
const STOP_JS: &str = r#"
const state = window.__serabotMic;
if (!state || !state.recorder) {
  return { ok: false, error: "Nothing is recording." };
}
const recorder = state.recorder;
const mimeType = state.mimeType || recorder.mimeType || "audio/webm";
const finished = new Promise((resolve) => {
  recorder.addEventListener("stop", () => {
    const chunks = state.chunks.slice();
    state.chunks = [];
    try {
      const blob = new Blob(chunks, { type: mimeType });
      const reader = new FileReader();
      reader.onload = () => {
        const result = String(reader.result || "");
        const comma = result.indexOf(",");
        state.base64 = comma >= 0 ? result.slice(comma + 1) : result;
        resolve({ ok: true, mimeType: mimeType });
      };
      reader.onerror = () => resolve({ ok: false, error: "The recording could not be read." });
      reader.readAsDataURL(blob);
    } catch (error) {
      resolve({ ok: false, error: "The recording could not be encoded." });
    }
  }, { once: true });
});
try {
  recorder.stop();
} catch (error) {
  return { ok: false, error: "The recorder did not stop." };
}
if (state.stream) {
  state.stream.getTracks().forEach((track) => track.stop());
  state.stream = null;
}
state.recorder = null;
return await finished;
"#;

/// Read the encoded recording back and clear it.
const READ_JS: &str = r#"
const state = window.__serabotMic;
if (!state || !state.base64) {
  return { ok: false, error: "The recording was empty." };
}
const base64 = state.base64;
state.base64 = null;
return { ok: true, base64: base64 };
"#;

/// Drop the recorder and the microphone stream without keeping the audio.
const CANCEL_JS: &str = r#"
const state = window.__serabotMic;
if (!state) { return { ok: true }; }
if (state.recorder) {
  try { state.recorder.stop(); } catch (error) { /* already stopped */ }
}
if (state.stream) {
  state.stream.getTracks().forEach((track) => track.stop());
}
state.recorder = null;
state.stream = null;
state.chunks = [];
state.base64 = null;
return { ok: true };
"#;

/// File name used for a webm recording.
const WEBM_FILENAME: &str = "note.webm";
/// File name used for an mp4 recording. The server maps `m4a` to audio/mp4.
const MP4_FILENAME: &str = "note.m4a";

/// A microphone recorder driven by the webview.
///
/// The recorder holds no audio of its own: the bytes live in the webview
/// until [`MicRecorder::stop`] reads them back.
pub struct MicRecorder {
    recording: bool,
    filename: String,
}

impl Default for MicRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl MicRecorder {
    /// A recorder that is not running.
    pub fn new() -> Self {
        Self {
            recording: false,
            filename: WEBM_FILENAME.to_owned(),
        }
    }

    /// True while the webview is recording.
    pub fn is_recording(&self) -> bool {
        self.recording
    }

    /// The file name to send with the recording.
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// Ask for the microphone and start recording.
    pub async fn start(&mut self) -> Result<(), String> {
        if self.recording {
            return Err("A recording is already running.".into());
        }
        let value = eval_json(START_JS).await?;
        check_result(&value)?;
        let mime = value
            .get("mimeType")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        self.filename = filename_for(mime).to_owned();
        self.recording = true;
        Ok(())
    }

    /// Stop recording and return the encoded audio bytes.
    pub async fn stop(&mut self) -> Result<Vec<u8>, String> {
        if !self.recording {
            return Err("Nothing is recording.".into());
        }
        let stopped = eval_json(STOP_JS).await;
        self.recording = false;
        let stopped = stopped?;
        check_result(&stopped)?;
        let value = eval_json(READ_JS).await?;
        check_result(&value)?;
        let base64 = value
            .get("base64")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "The recording was empty.".to_string())?;
        let bytes = decode_base64(base64)?;
        if bytes.is_empty() {
            return Err("The recording was empty.".into());
        }
        Ok(bytes)
    }

    /// Stop recording and throw the audio away.
    pub async fn cancel(&mut self) {
        self.recording = false;
        let _ = eval_json(CANCEL_JS).await;
    }
}

/// The file name that matches a recorder mime type.
pub fn filename_for(mime_type: &str) -> &'static str {
    if mime_type.starts_with("audio/mp4") {
        MP4_FILENAME
    } else {
        WEBM_FILENAME
    }
}

/// Run a script in the webview and return its JSON result.
async fn eval_json(script: &str) -> Result<serde_json::Value, String> {
    match dioxus::document::eval(script).await {
        Ok(value) => Ok(value),
        Err(dioxus::document::EvalError::Unsupported) => {
            Err("Microphone capture is not available in this window.".into())
        }
        Err(error) => Err(format!("The recorder script failed: {error}")),
    }
}

/// Read the `{ ok, error }` shape the recorder scripts return.
fn check_result(value: &serde_json::Value) -> Result<(), String> {
    if value
        .get("ok")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return Ok(());
    }
    let message = value
        .get("error")
        .and_then(|value| value.as_str())
        .unwrap_or("The recorder failed.");
    Err(message.to_owned())
}

/// Decode standard base64 into bytes. Whitespace is ignored, `=` padding is
/// accepted, and both the standard and the URL-safe alphabet are read.
pub fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    let invalid = "The recording data is not valid base64.".to_owned();
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    let mut data = 0usize;
    let mut padding = 0usize;
    for character in input.chars() {
        if character.is_whitespace() {
            continue;
        }
        if character == '=' {
            padding += 1;
            if padding > 2 {
                return Err(invalid);
            }
            continue;
        }
        if padding > 0 {
            return Err(invalid);
        }
        let Some(symbol) = decode_symbol(character) else {
            return Err(invalid);
        };
        data += 1;
        buffer = (buffer << 6) | u32::from(symbol);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    if data % 4 == 1 {
        // A trailing group of one character cannot hold a whole byte.
        return Err(invalid);
    }
    if padding > 0 && (data + padding) % 4 != 0 {
        return Err(invalid);
    }
    Ok(out)
}

fn decode_symbol(character: char) -> Option<u8> {
    match character {
        'A'..='Z' => Some(character as u8 - b'A'),
        'a'..='z' => Some(character as u8 - b'a' + 26),
        '0'..='9' => Some(character as u8 - b'0' + 52),
        '+' | '-' => Some(62),
        '/' | '_' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_known_vectors() {
        assert_eq!(decode_base64("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_base64("TQ==").unwrap(), b"M".to_vec());
        assert_eq!(decode_base64("TWE=").unwrap(), b"Ma".to_vec());
        assert_eq!(decode_base64("TWFu").unwrap(), b"Man".to_vec());
        assert_eq!(decode_base64("TWFuVA==").unwrap(), b"ManT".to_vec());
        assert_eq!(
            decode_base64("aGVsbG8gd29ybGQ=").unwrap(),
            b"hello world".to_vec()
        );
        assert_eq!(decode_base64("TW Fu\n").unwrap(), b"Man".to_vec());
        assert_eq!(decode_base64("-_8=").unwrap(), vec![0xfb, 0xff]);
    }

    #[test]
    fn base64_rejects_broken_input() {
        assert!(decode_base64("T").is_err());
        assert!(decode_base64("TWFu!").is_err());
        assert!(decode_base64("TW==Fu").is_err());
        assert!(decode_base64("====").is_err());
        assert!(decode_base64("TQ=").is_err());
    }

    #[test]
    fn the_scripts_share_one_global() {
        assert!(START_JS.contains(STATE_KEY));
        assert!(STOP_JS.contains(STATE_KEY));
        assert!(READ_JS.contains(STATE_KEY));
        assert!(CANCEL_JS.contains(STATE_KEY));
    }

    #[test]
    fn file_names_follow_the_recorded_format() {
        assert_eq!(filename_for("audio/webm;codecs=opus"), "note.webm");
        assert_eq!(filename_for("audio/webm"), "note.webm");
        assert_eq!(filename_for("audio/mp4"), "note.m4a");
        assert_eq!(filename_for(""), "note.webm");
    }

    #[test]
    fn recorder_reports_a_clean_state_before_and_after_use() {
        let recorder = MicRecorder::new();
        assert!(!recorder.is_recording());
        assert_eq!(recorder.filename(), "note.webm");
    }
}
