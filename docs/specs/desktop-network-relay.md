# Desktop network relay (0.66)

Status: **built in 0.66**, as designed below. The owner chose: opt-in per
provider; while the desktop is offline the route simply fails over.

The transport is a long-poll exchange over the desktop's existing
authenticated API connection (`POST /v1/relay/net/exchange`) rather than a
WebSocket, because the desktop already runs long-polls this way and needs no
new dependency. Frames are `open`, `opened`, `data` (base64) and `close`.

## Goal

Let a member choose to send some of the server's outbound model traffic
through their own Windows desktop, so the provider sees the member's home
connection instead of the server's data-centre address. The owner's case:
Google Antigravity (and similar plan sign-ins) refuse or throttle requests
from cloud IPs but accept them from a personal PC.

It is the member's own account, used from the member's own computer. The
relay does not hide who is calling. It only changes which network the request
leaves from. A provider's terms may still forbid automated use of a consumer
plan; that risk is the member's, and the console says so the same way the
plan sign-in risk banner does.

## Feasible, with one constraint

It works while the desktop app is open and online. When the desktop is closed,
the route falls back to the server's own network or fails over to the next
route in the bot's chain. It never silently queues.

## Shape: a TCP tunnel, not an HTTP proxy

```
server (reqwest) ──HTTP CONNECT──▶ 127.0.0.1:<per-user port>  (server, loopback only)
        │                                   │
        │                          frames over the desktop's
        │                          authenticated WebSocket
        ▼                                   ▼
     TLS to provider  ◀── raw bytes ──  desktop opens TCP to provider:443
```

- **The desktop dials out.** It opens `wss://<server>/v1/relay/net` with its
  session token, the same way it already talks to the API. No port is opened
  on the member's PC.
- **The server runs a CONNECT proxy on loopback** for that member. Provider
  clients flagged "via my desktop" get
  `reqwest::Proxy::https("http://127.0.0.1:<port>")`.
- **TLS stays end to end** between the server and the provider. The desktop
  only moves encrypted bytes: it never sees API keys, tokens, prompts or
  replies.
- **Streams are multiplexed** over the one WebSocket: open, data and close
  frames keyed by a stream id. Streaming replies (SSE) work because the
  bytes stream as they arrive.

## Limits (the security design)

| Rule | Why |
| --- | --- |
| Off by default; turned on per provider route by the member, in the console | Opt-in, visible |
| Destinations limited to a fixed list per provider (e.g. `cloudcode-pa.googleapis.com`, `oauth2.googleapis.com` for Antigravity), port 443 only | The desktop never becomes a general proxy into the home network |
| The desktop refuses private, loopback and link-local addresses after its own DNS lookup | No pivot into the LAN, even if the list were wrong |
| One member's relay carries only that member's routes | UID-scoped like everything else |
| The loopback proxy binds 127.0.0.1 and requires a per-session secret in `Proxy-Authorization` | Other processes on the server cannot borrow it |
| Byte and stream caps per minute; idle streams close after 60 s | Bounded |
| Every opened stream is audited (destination host, bytes, duration; never content) | Owner can see what used the relay |
| Desktop shows a "Relaying for SeraBot" indicator with a one-click stop | The member always knows |

## Pieces to build (0.66)

1. Server: `relay_net` module with the loopback CONNECT listener per member,
   the WebSocket route, frame codec, caps and audit.
2. Router: a per-route `egress = "server" | "desktop"` setting. Clients built
   for a desktop route use the relay proxy; when the desktop is offline, the
   route reports "desktop offline" and the chain fails over.
3. Desktop: a WebSocket client that opens TCP to allowed hosts, the address
   checks, the indicator and the stop button, plus a Settings switch per
   provider.
4. Tests: frame codec, allowlist and private-address refusal, offline
   failover, and a loopback end-to-end test with a local TLS server.

## As built

- Server: `crates/serabot/src/relay_net.rs` (hub, loopback CONNECT listener,
  settings, address rule), `api/relay.rs` (`GET/PUT /v1/relay/net`,
  `POST /v1/relay/net/exchange`), migration `0043_relay_settings.sql`, and the
  proxy hook in `providers::open_provider_client`.
- Desktop: `crates/serabot-desktop/src/net_relay.rs` (two threads, host list,
  public-address check), `ui_net_relay.rs` (card under Settings → Local
  folders).
- Relayable providers: Google Antigravity, Google Gemini API, Anthropic,
  OpenAI. Each has a fixed host list on both sides.
