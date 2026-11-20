# Release 0.67.0

Usage insights: a new **Insights** section on the Usage tab. It shows where
you talk to your bots, what the PC tunnel carried, how fast and how reliable
each model is, and (for the owner) the server's load over the last day. Tests
use synthetic fixtures. Nothing here ran against a live provider, a real
desktop or a phone.

## Where you talk to your bots

- Turns are counted per channel: this PC app, the phone app, Telegram,
  WhatsApp, API and scripts, and schedules and watches. Each row shows the
  turns, the share of all turns, and the tokens those turns used.
- The desktop sends `X-SeraBot-Client: desktop` and the phone app sends
  `X-SeraBot-Client: mobile`. The server records this per turn in a new
  `client_turns` table (migration 0044). The table holds counts only, never
  message text.
- App turns sent before 0.67 have no client tag. They show as "App (before
  0.67)".

## PC tunnel

- The share of model calls that went through this PC, plus bytes sent and
  received and the number of tunnels.
- A chart of traffic per day, the busiest hosts with their average tunnel
  duration, and whether the relay is live right now.
- The data comes from the existing `relay.tunnel` audit rows. Each
  `model.used` audit row now also records `relayed`.

## Model speed and reliability

- `model.used` audit rows now record `latency_ms`, the wall time of the model
  call including retries.
- The table shows, per model: calls, average latency, the slowest 5% (p95),
  the share of calls sent via this PC, and failovers away from that model.
  Failovers come from `model.failover` audits.

## Server load (owner only)

- CPU and memory are sampled every 5 minutes and kept in memory for 24 hours.
  Nothing is stored. The samples are drawn as sparklines with the current and
  peak values.

## API

- `GET /v1/usage/insights?days=1..90&scope=me|all` returns `channels`,
  `tunnel`, `relay_online`, `models` and `resources`. `scope=all` is admin
  only, and `resources` is admin only.
