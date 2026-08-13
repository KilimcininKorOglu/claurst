# claurst-relay

A self-hosted relay that lets a phone or browser drive a claurst session running on your machine.

The CLI dials out and long-polls, so your machine needs no inbound port and no firewall change. The relay only queues and forwards; it does not interpret prompts, events or code.

```
phone/web  ──HTTP+SSE──►  relay (Docker)  ◄──long-poll──  claurst (your machine)
```

## Before you run it

This token is a remote command-execution credential. Anything holding it can send a prompt into a running claurst session, and that session executes tools on your machine.

- The relay does not terminate TLS. Put a TLS-terminating reverse proxy in front of it, or reach it only over a VPN or LAN. Without TLS the token and your source travel in plaintext.
- `docker-compose.yml` publishes on `127.0.0.1` by default for that reason. Changing it to `0.0.0.0` without TLS in front puts the token on the wire.
- The token must be at least 32 characters. The relay refuses to start below that rather than running with a weak secret.

## Running

```bash
cp .env.example .env
openssl rand -hex 32          # paste into RELAY_TOKEN
docker compose up -d
```

Then point claurst at it, in `~/.claurst/settings.json`:

```json
{
  "remoteControl": {
    "url": "https://relay.example",
    "token": "the same token",
    "permissionMode": "ask",
    "label": "workstation"
  }
}
```

`CLAURST_BRIDGE_URL` and `CLAURST_BRIDGE_TOKEN` override the settings file, which is handy while developing.

## Configuration

| Variable                 | Default          | Meaning                                            |
|--------------------------|------------------|----------------------------------------------------|
| `RELAY_TOKEN`            | none, required   | Shared secret; at least 32 characters              |
| `RELAY_BIND`             | `0.0.0.0:8350`   | Listen address inside the container                |
| `RELAY_SESSION_TTL_SECS` | `900`            | Drop a session after this long without a poll      |
| `RELAY_EVENT_BUFFER`     | `500`            | Events retained per session for replay             |
| `RELAY_INBOUND_QUEUE`    | `100`            | Messages queued for a runner before the oldest goes |
| `RUST_LOG`               | `claurst_relay=info` | Log filter                                     |

Host port 8350 is reserved for this project in the central Docker port registry.

## API

Two surfaces, deliberately separate.

**Runner surface** — fixed by what `claurst-bridge` already calls, so it cannot change:

| Method   | Path                                        |
|----------|---------------------------------------------|
| `POST`   | `/api/claude_code/sessions`                 |
| `GET`    | `/api/claude_code/sessions/{id}/poll`       |
| `POST`   | `/api/claude_code/sessions/{id}/events`     |
| `DELETE` | `/api/claude_code/sessions/{id}`            |

The CLI also runs a second, best-effort path under `/api/bridge/sessions`. It carries a subset of the same events, so the relay accepts and discards it; its message endpoint deliberately returns an empty array, because answering there too would deliver every prompt twice.

**Client surface** — ours, and what a native app would use:

| Method | Path                                             | Notes                                     |
|--------|--------------------------------------------------|-------------------------------------------|
| `POST` | `/api/client/auth`                               | Sets an `HttpOnly` cookie                 |
| `GET`  | `/api/client/sessions`                           | Open sessions, most recently active first |
| `GET`  | `/api/client/sessions/{id}/stream?since=<seq>`   | SSE; resumes from the ring buffer         |
| `POST` | `/api/client/sessions/{id}/prompt`               | `{"content": "..."}`                      |
| `POST` | `/api/client/sessions/{id}/permission`           | `{"request_id", "tool_use_id", "decision"}` |
| `POST` | `/api/client/sessions/{id}/cancel`               | Body optional                             |

`GET /healthz` needs no token.

Authentication accepts either a bearer token or the cookie. The cookie exists because a browser `EventSource` cannot set request headers, so the stream endpoint is unreachable without it.

## State

Everything is in memory. A restart drops the sessions and the CLI re-registers on its next poll. Nothing is written to disk, so the relay host never holds a durable copy of your code.

Each session keeps a bounded inbound queue and a sequence-numbered ring buffer of recent events. A client reconnecting passes its last sequence number and picks up from there.

## Development

```bash
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo test -- --test-threads=1
```

Tests drive the router through `tower::ServiceExt::oneshot` and make no network calls.

This is a standalone Cargo project, not a member of the `src-rust` workspace. It carries none of claurst's dependency tree, which keeps the image small and leaves the release process untouched.
