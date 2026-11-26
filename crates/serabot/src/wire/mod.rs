//! Provider wire formats beyond OpenAI chat completions.
//!
//! - [`responses`]: the OpenAI Responses transport (`POST {base}/responses`
//!   over SSE). Meta Muse Code and the Codex (ChatGPT plan) endpoint use it.
//! - [`anthropic`]: the native Anthropic Messages transport, for API-key
//!   accounts and (behind the admin flag) Claude plan sign-ins.
//! - [`schema`]: the tool-schema flattener both of them share.
//! - [`sse`]: a small server-sent-events reader.
//!
//! These modules are pure: they build request bodies and read finished
//! response bodies. The HTTP loop (credentials, refresh, retries, typed
//! failures) lives in `llm.rs`, which is also where every request is made, so
//! the gateway's egress and blocklist rules apply to them as they do to chat
//! completions.

pub mod anthropic;
pub mod responses;
pub mod schema;
pub mod sse;

/// Which request and response format a client speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wire {
    /// `POST {base}/chat/completions`.
    #[default]
    ChatCompletions,
    /// `POST {base}/responses` over SSE.
    Responses,
    /// `POST {base}/messages` with an `x-api-key`.
    AnthropicKey,
    /// `POST {base}/messages` with a Claude plan bearer token.
    AnthropicPlan,
    /// `POST {base}/v1/turn` on the local loopback Claude Agent SDK gateway.
    ClaudeGateway,
}

impl Wire {
    pub fn is_anthropic(self) -> bool {
        matches!(self, Self::AnthropicKey | Self::AnthropicPlan)
    }
}
