# Remote Control

Drive a claurst session from your phone or another browser, through a relay you host yourself.

The CLI dials out and long-polls. Your machine needs no inbound port, no port forward and no firewall change. The relay only queues and forwards; it never runs anything.

```
phone/web  ──HTTP+SSE──►  relay (Docker)  ◄──long-poll──  claurst (your machine)
```

---

## Read this before you start

The relay token is a remote command-execution credential. Anything holding it can send a prompt into a running session, and that session runs tools on your machine.

Three consequences, all enforced in code rather than left to you:

- The token must be at least 32 characters. The relay refuses to start below that, and claurst refuses to connect.
- The relay does not terminate TLS. Put a TLS-terminating reverse proxy in front of it, or reach it only over a VPN or LAN. Without TLS the token and your source travel in plaintext.
- `docker-compose.yml` publishes on `127.0.0.1` for that reason. Changing it to `0.0.0.0` without TLS in front puts the token on the wire.

The relay keeps everything in memory and writes nothing to disk, so the relay host never holds a durable copy of your code. It does see the transcript in transit; there is no end-to-end encryption.

---

## 1. Run the relay

The relay lives in `relay/` in this repository. It is a standalone Cargo project, not part of the `src-rust` workspace.

```bash
cd relay
cp .env.example .env
openssl rand -hex 32          # paste the output into RELAY_TOKEN
docker compose up -d
```

Check it is up:

```bash
curl http://127.0.0.1:8350/healthz     # -> ok
```

| Variable                 | Default              | Meaning                                             |
|--------------------------|----------------------|-----------------------------------------------------|
| `RELAY_TOKEN`            | none, required       | Shared secret; at least 32 characters               |
| `RELAY_BIND`             | `0.0.0.0:8350`       | Listen address inside the container                 |
| `RELAY_SESSION_TTL_SECS` | `900`                | Drop a session after this long without a poll       |
| `RELAY_EVENT_BUFFER`     | `500`                | Events retained per session for replay              |
| `RELAY_INBOUND_QUEUE`    | `100`                | Messages queued for a session before the oldest goes |
| `RUST_LOG`               | `claurst_relay=info` | Log filter                                          |

---

## 2. Point claurst at it

In your user settings file (`~/.claurst/settings.json`, or wherever `CLAURST_HOME` points):

```json
{
  "remoteControl": {
    "url": "https://relay.example",
    "token": "the same token you put in RELAY_TOKEN",
    "permissionMode": "ask",
    "label": "workstation"
  }
}
```

`label` is what the session list shows. Without it the hostname is used.

This block is read from the user settings file only. A project settings file cannot set it, because a repository should not be able to point your machine's bridge at a relay.

For a temporary redirect while developing, `CLAURST_BRIDGE_URL` and `CLAURST_BRIDGE_TOKEN` override the settings file.

---

## 3. Enable the bridge

```
/remote-control start
```

Restart claurst. The bridge connects on launch. `/remote-control` with no argument shows which relay it resolved, where each value came from, and whether the token is usable.

`/remote-control stop` disables it again.

---

## 4. Open the relay

Point a browser at the relay address and enter the token. Three views:

- **Token entry** — once per browser. The token goes into an `HttpOnly` cookie, so the page cannot read it back.
- **Session list** — every connected machine, most recently active first, with its label and working directory.
- **Session screen** — the live transcript, a prompt box, a stop button, and the permission card.

The layout starts at phone width and adapts upward, so a phone, a tablet and a desktop browser all get a usable screen. Session cards fill a grid once there is room for it, and the transcript stops widening past a readable measure instead of running the width of a monitor.

---

## Permissions

Two settings sit on different axes, and it is worth being clear about which does what.

| Setting                          | Question it answers               |
|----------------------------------|-----------------------------------|
| `config.permission_mode`         | Does a tool ask at all?           |
| `remoteControl.permissionMode`   | Who may answer, when one asks?    |

`config.permission_mode` runs first. In `bypassPermissions` (`--dangerously-skip-permissions`) every tool is allowed outright, and in `plan` every write is refused outright. Neither ever produces a prompt, so `remoteControl.permissionMode` is never consulted in those two modes. It cannot tighten anything: setting `local-only` under `bypassPermissions` protects you from nothing.

It matters in `default` and in `acceptEdits`, where tools still ask. `/remote-control` says which of the two situations you are in.

Note that sending a prompt is not a permission at all. Anything holding the relay token can start a turn regardless of the mode. Combined with `bypassPermissions` that means the token alone runs arbitrary tools on your machine with no approval step anywhere.

`permissionMode` decides who may answer when a tool asks for approval.

| Value          | Behaviour                                                                                   |
|----------------|---------------------------------------------------------------------------------------------|
| `ask`          | The request appears both in the terminal and on the phone. Either side may answer. Default.  |
| `local-only`   | The request appears in both places, but a remote answer is refused. Only the keyboard decides. |

Under `ask`, the phone is a security boundary. Anyone holding your unlocked phone can approve a tool call on your machine.

The card offers three answers:

| Button              | Effect                                                                 |
|---------------------|------------------------------------------------------------------------|
| Allow once          | This call only.                                                        |
| Allow this session  | This tool for the rest of the session. Nothing is written to settings. |
| Deny                | Refuse the call.                                                       |

A remote tap never writes a permanent rule into your settings file. Persistent allows are a keyboard-only decision.

---

## What survives a restart

Nothing on the relay. Sessions, queues and buffers are in memory, so a relay restart drops them and the CLI re-registers on its next poll.

Each session keeps a bounded ring buffer of recent events. A browser that lost its connection reconnects with the last sequence number it saw and resumes from there. Once events fall out of the buffer they are gone; the terminal transcript remains complete.

A session whose runner stops polling is swept after `RELAY_SESSION_TTL_SECS`.

---

## Building a different client

The relay speaks two separate protocols on purpose.

The **runner surface** (`/api/claude_code/sessions/...`) is fixed by what the CLI already calls and cannot change.

The **client surface** is ours, and a native app should use it:

| Method | Path                                           | Notes                                       |
|--------|------------------------------------------------|---------------------------------------------|
| `POST` | `/api/client/auth`                             | Sets the cookie                             |
| `GET`  | `/api/client/sessions`                         | Open sessions, most recently active first   |
| `GET`  | `/api/client/sessions/{id}/stream?since=<seq>` | SSE; resumes from the ring buffer           |
| `POST` | `/api/client/sessions/{id}/prompt`             | `{"content": "..."}`                        |
| `POST` | `/api/client/sessions/{id}/permission`         | `{"request_id", "tool_use_id", "decision"}` |
| `POST` | `/api/client/sessions/{id}/cancel`             | Body optional                               |

Authentication accepts a bearer token or the cookie. A native client should use the bearer token; the cookie exists because a browser `EventSource` cannot set request headers.

`GET /healthz` needs no token.

---

## Troubleshooting

**The session list is empty.** The CLI has not registered. Check `/remote-control` in the terminal: it prints the relay it resolved and whether the token is usable. A token under 32 characters stops the bridge from starting at all, and says so.

**The stream reconnects in a loop.** The session was swept for going quiet, or the relay restarted. Go back to the session list and reopen it.

**A prompt is accepted but nothing happens.** The session is waiting on a permission request. Under `local-only`, only the terminal can clear it.

**Nothing loads over HTTPS.** The relay does not terminate TLS. The reverse proxy in front of it must, and should set `X-Forwarded-Proto: https` so the session cookie is marked `Secure`.
