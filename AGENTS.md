# AGENTS.md
This file provides guidance to AI coding agents (Claude Code, Cursor, Codex, Gemini CLI, GitHub Copilot, Devin, Windsurf, Amp, Jules, Aider, VS Code, Zed, goose, RooCode, etc.) when working with code in this repository.

Agent-facing rules for working on Claurst. When a rule here disagrees with a rule stored closer to the code, the rule closer to the code wins.

## Project Overview

Claurst is an open-source, multi-provider terminal coding agent written in Rust. It is a clean-room reimplementation of Claude Code's behavior: `spec/` holds the behavioral specification, `src-rust/` holds the implementation written from that spec alone. No proprietary TypeScript source is present in this repo, and none may be introduced.

## Repository Layout

| Path | Contents |
|------|----------|
| `src-rust/` | The Cargo workspace (12 `claurst-*` crates). **All cargo commands for the CLI run from here.** |
| `relay/` | The self-hosted relay that carries remote-control sessions: axum + tokio, plus the `static/` web client. A **separate Cargo project with its own `Cargo.lock`**, deliberately not a workspace member, so cargo run from `src-rust/` never builds or tests it. |
| `spec/` | Clean-room behavioral specification (`00_overview.md` … `13_rust_codebase.md`). Reference only, not code. |
| `docs/`, `index.html`, `session/`, `public/` | GitHub Pages site (`kilimcininkoroglu.github.io/claurst`), deployed by `.github/workflows/pages.yml`. No `CNAME`: the site is served from the default Pages domain. |
| `npm/` | The `claurst` npm wrapper; `install.js` postinstall downloads the prebuilt binary. |
| `install.sh`, `install.ps1` | One-liner installers served from GitHub Releases. |
| `scripts/bump-version.py` | The only supported way to change the version. |
| `.devcontainer/` | VS Code devcontainer (`rust:1-bullseye` base). |

There is no Makefile and no JS/TS build step; `npm/` is packaged as-is, and the relay's web client is plain HTML/CSS/JS served straight from `relay/static/`.

Two Cargo projects means two lockfiles and two `target/` directories. A change under `relay/` never affects a `src-rust/` build and vice versa. `.github/workflows/ci.yml` covers both: the `test` job builds the workspace on three platforms, and a separate `relay` job runs the relay's tests, clippy and rustfmt on Linux.

## Build & Run Commands

Run from `src-rust/` unless noted.

| Task | Command |
|------|---------|
| Type-check (after any Rust change) | `cargo check --workspace` |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` |
| Format | `cargo fmt --all` |
| Test everything | `cargo test --workspace -- --test-threads=1` |
| Test one crate | `cargo test --package claurst-<crate>` |
| Test one test | `cargo test --package claurst-<crate> -- <pattern>` |
| Debug build | `cargo build` |
| Release build | `cargo build --release --package claurst` |
| Build without voice/ALSA | `cargo build --release --package claurst --no-default-features` |
| Run interactively | `cargo run -- "test prompt"` |
| Run headless | `cargo run -- --print "test"` |
| Stamp a version | `python scripts/bump-version.py vX.Y.Z` (from repo root) |

- Fix every error **and every warning** from `cargo check` before committing.
- Do not add `#[allow(...)]` to silence clippy without a written justification.
- Avoid `cargo build --release` / `cargo run --release` unless you specifically need optimised output — debug builds and `cargo check` are 10× faster.
- If you create or modify a test, run it and iterate until it passes.
- Prefer `--print` mode for verifying non-TUI logic; it is faster and does not block.
- Don't run blocking interactive commands you can't exit — the agent will hang. If you must, capture output with `--print` mode or pipe into `head`.

### Relay commands

Run from `relay/`. The relay has no `--workspace` because it is a single crate.

| Task | Command |
|------|---------|
| Lint | `cargo clippy --all-targets -- -D warnings` |
| Test | `cargo test -- --test-threads=1` |
| Run locally | `RELAY_TOKEN=<32+ chars> RELAY_BIND=127.0.0.1:8350 cargo run` |
| Run in Docker | `cp .env.example .env`, set `RELAY_TOKEN`, then `docker compose up -d` |

- `RELAY_TOKEN` must be at least 32 characters; the relay refuses to start below that, and `claurst_core::config::MIN_REMOTE_TOKEN_LEN` enforces the same bound on the client side.
- `relay/src/web.rs` embeds `relay/static/` with `include_str!`. After editing any static file you must rebuild, restart the process, **and** reload the browser page — the running process keeps serving the assets it was compiled with, which makes a stale page look like a failed change.
- `docker-compose.yml` publishes on `127.0.0.1` on purpose: the relay does not terminate TLS. Do not change it to `0.0.0.0` without a TLS-terminating proxy in front.

### CI expectations

`.github/workflows/ci.yml` triggers on any change under `src-rust/**` or `relay/**` and runs two jobs.

`test`, on `ubuntu-latest`, `windows-latest`, and `macos-latest`:

- Tests: `cargo test --workspace --locked -- --test-threads=1`. **Serial execution is required** — several tests mutate process-global state (`HOME`, `ANTHROPIC_API_KEY`, `XDG_CONFIG_HOME`) and race under parallelism.
- Clippy: enforced with `-D warnings`, Linux only.
- rustfmt: `cargo fmt --all --check` is **advisory** in CI (`continue-on-error`), because CRLF-terminated files can make it disagree across runners. The tree itself is fmt-clean, so run `cargo fmt --all` before committing and treat any remaining diff as yours.

`relay`, on `ubuntu-latest` only, because the relay is a plain axum service with no platform-conditional code: `cargo test --locked -- --test-threads=1`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all --check` **enforced** rather than advisory.

### Testing the TUI in a controlled terminal

The ratatui frontend is sensitive to terminal size and key encoding. For repeatable manual tests use tmux:

```bash
# Build a debug binary once
cargo build

# 80×24 session
tmux new-session -d -s claurst-test -x 80 -y 24
tmux send-keys -t claurst-test "./target/debug/claurst" Enter

# Give it time to redraw, then capture
sleep 2 && tmux capture-pane -t claurst-test -p

# Drive input
tmux send-keys -t claurst-test "your prompt here" Enter
tmux send-keys -t claurst-test Escape
tmux send-keys -t claurst-test C-o   # ctrl+o

# Cleanup
tmux kill-session -t claurst-test
```

On Windows hosts, prefer `cargo run -- --print "..."` against the headless path. The Windows console has known quirks with the kitty keyboard protocol — see `crates/tui` for the push/pop workaround.

## Environment Setup

- Linux build dependencies: `libasound2-dev` and `pkg-config` (ALSA headers for the `voice` feature's `cpal` dependency). `cmake` is needed by BoringSSL via `wreq` and is preinstalled on GitHub runners.
- Headless servers and Raspberry Pi: build with `--no-default-features` to drop voice support and the ALSA requirement.
- Devcontainer: open the repo in VS Code and "Reopen in Container". It preinstalls `gnupg2`, `libasound2-dev`, `libxdo-dev`, `pkg-config`; binds `./.claurst` to `/home/vscode/.claurst`; and prepends `src-rust/target/debug` and `src-rust/target/release` to `PATH`. GPG and SSH forwarding work if configured on the host.
- Runtime credentials: set a provider env var (e.g. `ANTHROPIC_API_KEY`) or configure `settings.json` via `/connect`. Never commit credentials.

## Architecture Overview

Twelve crates, strictly one-directional dependencies with `core` at the bottom:

```
core ← api ← tools ← query ← {tui, commands, acp, bridge} ← cli (binary)
                 ↖ mcp ↙        ↖ plugins ↙
```

| Crate | Responsibility |
|-------|----------------|
| `claurst-core` | Shared foundation: `Settings`/`Config`, `Message`/`ContentBlock`, `ClaudeError`, permissions, auth store, session storage (JSONL + SQLite), memory-file loading, keybindings, snapshots, goals, feature gates. |
| `claurst-api` | Provider abstraction. `provider.rs` defines `LlmProvider`; `providers/` holds one file per wire format; `registry.rs` maps `ProviderId` → `Arc<dyn LlmProvider>`; `model_registry.rs` holds model IDs and capability metadata; `transformers/` and `protocol/` convert message shapes. |
| `claurst-tools` | Every model-callable tool (PTY bash, file read/write/edit, glob, grep, web fetch/search, MCP resources, tasks, teams, skills, cron, computer-use). Each implements the `Tool` trait; `ToolContext` carries cwd, permission mode, and the `PermissionHandler`. |
| `claurst-query` | The agentic loop: send → stream → detect `tool_use` → dispatch → feed results back. Also auto/micro-compaction, subagents, coordinator/worker orchestration, `/goal` multi-turn continuation, cron scheduling, session-memory extraction. Emits `QueryEvent` over a tokio channel. |
| `claurst-tui` | ratatui + crossterm frontend. `app.rs` holds `App` state and the event loop and consumes `QueryEvent`. Terminal setup/teardown lives in `lib.rs` and is platform-conditional. |
| `claurst-commands` | Slash-command implementations (`/connect`, `/mcp`, `/export`, `/share`, `/memory`, `/goal`, …), one module per area. |
| `claurst-mcp` | MCP client over `rmcp` (stdio + streamable HTTP), server registry, OAuth, trust prompts. |
| `claurst-plugins` | Plugin discovery, manifest parsing, and the hook registry (`PreToolUse`, `SessionStart`, …) invoked by the query runner. |
| `claurst-acp` | Agent Client Protocol server (`claurst acp`), JSON-RPC 2.0 over stdio for Zed and other editors. |
| `claurst-bridge` | Remote-control bridge (long-polling session protocol) for web/mobile-initiated sessions. |
| `claurst-buddy` | The companion shown beside the input box, reached through `/buddy`. Its body derives deterministically from a seeded PRNG; its model-written "soul" persists in `companion.json`. Not the welcome-screen mascot, which is `crates/tui/src/mikmik.rs`. |
| `claurst-cli` | The `claurst` binary. Parses ~40 clap flags and dispatches to TUI, headless `--print`, or a subcommand. |

Files that are large enough to need a map before editing:

- `crates/core/src/lib.rs` (~5.3k lines) — the `config`, `error`, `types`, and `permissions` modules are defined **inline** here, not as separate files. Grep `lib.rs` before concluding a module is missing.
- `crates/tui/src/app.rs` (~7.8k lines) — `App` state plus the event loop.
- `crates/cli/src/main.rs` (~4.9k lines) — intercepts the `auth`, `upgrade`, and `acp` subcommands from raw argv **before** clap parses. The base system prompt is `include_str!("system_prompt.txt")`.

## Key Patterns

- **Config root.** Everything persisted lives under one directory, resolved by `Settings::config_dir()` in `crates/core/src/lib.rs`: `$CLAURST_HOME` → an existing legacy `~/.claurst` → `$XDG_CONFIG_HOME/claurst` → `~/.config/claurst`. Never hardcode `~/.claurst`; call `claurst_core::claurst_home()`.
- **Memory files.** `crates/core/src/claudemd.rs` loads enterprise → user → project → `{project_root}/.claurst/`. At each scope `AGENTS.md` loads first and `CLAUDE.md` second; both may exist and are additive.
- **Keybindings.** Never hardcode a key check inline (e.g. `key == KeyCode::Char('s') && mods.ctrl()`). All keybindings flow through `crates/core/src/keybindings.rs` — add a default there.
- **Two independent flag systems.** Compile-time Cargo features declared in `crates/core/Cargo.toml` and forwarded by `crates/tui/Cargo.toml` (`ultraplan`, `teammem`, `bridge_mode`, `voice`, …), *and* runtime env gates in `crates/core/src/feature_gates.rs` (`CLAURST_FEATURE_<NAME>`, `CLAURST_DYNAMIC_CONFIG_<NAME>`). Know which one you are touching.
- **The turn loop has two dispatch arms.** `run_query_loop` in `crates/query/src/lib.rs` chooses with `use_provider_dispatch` (`provider_id != "anthropic" || client.api_key_is_empty()`): one arm goes through the `LlmProvider` registry and fails with `ProviderError`, the other uses the raw Anthropic client and fails with `ClaudeError`. Each builds its own request and handles its own errors, so a turn-level policy — fallback switching, retries, budgets — must be added to **both** or it silently applies to only some providers.
- **Classify an API failure by its error type, never by its message.** `ProviderError` and `ClaudeError` both answer `is_retryable`. Matching on `Display` text is how a 429 was missed for a long time: it renders as `[openai] Rate limited` and `Rate limit exceeded`, neither of which contains the substring `rate_limit`.
- **`QueryEvent` has two consumers.** `crates/tui/src/app.rs` renders them; `crates/cli/src/main.rs` maps them onto `BridgeOutbound` for remote clients. Adding a variant or a field means touching both, and a remote client only learns what that mapping forwards.
- **Remote and keyboard answers must share one settle point.** A permission goes through `settle_pending_permission`, a question through `AskUserDialogState::answer_externally`, an MCP trust decision through `app.handle_mcp_approval_decision`, a rename through `apply_session_rename` — all in `crates/cli/src/main.rs` unless noted. Never answer one of these from a second place; the two paths drift.
- **A new `Settings` key needs five edits, not one.** `save_to_path_sync` serialises the typed struct, so a key with no field on `Settings` is silently dropped on the next write. Adding one means: the field on `Settings`, the copy into `Config` in `effective_config`, the entry in the `Settings`/`Config` merge functions, and — if the TUI is to toggle it — five more points in `crates/tui/src/settings_screen.rs` (struct field, `new()`, `apply_settings_from_snapshot`, `all_entries`, `toggle_or_cycle_current`). A command that writes only the settings file leaves the running session on the old value until the next launch; write the live `Config` too and return `ConfigChangeMessage`.
- **A new `SystemPromptOptions` field must reach `--dump-system-prompt`.** That flag promises to print exactly what a run sends, and it builds its own `QueryConfig` in `crates/cli/src/main.rs` rather than going through the REPL. A field set only on the turn path makes the dump quietly wrong.
- **ACP logging.** Write ACP logs to stderr only; anything on stdout corrupts the JSON-RPC protocol. `CLAURST_ACP_LOG=debug` enables verbose output.
- **Relay web client.** Render agent output with `textContent`, never `innerHTML`. Report a failure through `setStatus` and an in-turn message through `notice`; never call `alert`, `confirm`, or `prompt`, because a modal browser dialog is unusable on iOS. Serve any dependency the client needs from `relay/static/`, never a CDN — the relay is self-hosted and often reached over a VPN or LAN.
- **Generated files — never modify directly.** `src-rust/Cargo.lock` (regenerated by cargo; version bumps go through `scripts/bump-version.py`) and the `version` field in `npm/package.json` (also stamped by `bump-version.py`).

## Code Quality

- Read files in full before making wide-ranging changes, before editing files you have not already fully inspected, and when the user asks you to investigate or audit something. Do not rely only on search snippets for broad changes.
- No `.unwrap()` / `.expect()` on fallible operations in production paths — propagate via `Result` or pattern-match. `unwrap` is acceptable in tests and in cases where the invariant is statically obvious and commented.
- Avoid speculative `.clone()` — borrow first, clone only when ownership is actually needed. Same applies to `.to_string()` on `&str`.
- No `unsafe` blocks without a `// SAFETY:` comment explaining the invariant.
- Single-line helper functions with a single call site are forbidden; inline them instead.
- Don't guess external API shapes. Read the crate source under `~/.cargo/registry/` or check `cargo doc --open`. For Anthropic / OpenAI / Google wire formats, the `crates/api/src/providers/<provider>.rs` files are the authoritative reference inside this repo.
- **NEVER use type erasure to silence the compiler** — no `Box<dyn Any>`, no `serde_json::Value` shoved through a typed boundary just because the right type is annoying to derive. If a type is hard to express, ask the user.
- NEVER remove or downgrade code to fix compiler errors from outdated dependencies; bump the dependency in `Cargo.toml` or `src-rust/Cargo.toml` workspace deps instead.
- Always ask before removing functionality or code that appears to be intentional.
- Do not preserve backward compatibility unless the user explicitly asks for it.

### Dependency notes

- `wreq` / `wreq-util` are pinned to release-candidate versions (`=6.0.0-rc.29` / `=3.0.0-rc.12`). No stable release ships the TLS-impersonation API the Anthropic path relies on. Do not unpin them until 6.0.0 / 3.0.0 are stable (tracked as #227).
- TLS uses rustls rather than native-tls/openssl so it can coexist with wreq/BoringSSL without `openssl-sys ↔ boring-sys` symbol conflicts.
- The release profile keeps the default `panic = "unwind"`: the TUI panic hook relies on unwinding to restore the terminal (raw mode, cursor).

## Conversational Style

- Keep answers short and concise
- No emojis in commits, issues, PR comments, or code
- No fluff or cheerful filler text
- Technical prose only, be kind but direct (e.g., "Thanks @user" not "Thanks so much @user!")
- When the user asks a question, answer it first before making edits or running implementation commands.

## Adding a Provider

### 1. Provider identifier (`crates/core/src/provider_id.rs`)

Add a well-known constant on `ProviderId`, e.g. `pub const FOO: &'static str = "foo";`. Use the canonical name the provider publishes for its API.

### 2. Provider implementation (`crates/api/src/providers/`)

- OpenAI-compatible: add an entry to `openai_compat_providers.rs`. This is one line + an optional base-URL helper.
- Custom wire format: create `crates/api/src/providers/<name>.rs` exposing a struct that implements `LlmProvider` (see `provider.rs`). Mirror the structure of `anthropic.rs` or `google.rs` — request shaping, response parsing, streaming SSE handling, tool conversion.
- Add `pub mod <name>; pub use <name>::<Name>Provider;` to `providers/mod.rs`.

### 3. Register the provider (`crates/api/src/registry.rs`)

Import the new provider and add it to the registry construction. The registry hands back `Arc<dyn LlmProvider>` by id.

### 4. Model registry (`crates/api/src/model_registry.rs`)

Add the canonical model IDs and capability metadata (context window, supports thinking, supports vision, etc.).

### 5. Auth & env detection (`crates/core/src/auth_store.rs`, related)

If the provider uses an env var (e.g. `FOO_API_KEY`), wire it into the auth-store probe. For OAuth-style providers, see `codex_oauth.rs` and `device_code.rs` for the existing patterns.

### 6. Tests

- Add a smoke test in `crates/api/tests/` that exercises request shaping and response parsing against a mocked HTTP body. No live API calls — use the fixture pattern that the existing provider tests follow.
- If the provider supports tool calls, add a tool-call round-trip fixture.
- For OpenAI-compatible providers, the shared test in `crates/api/tests/openai_compat.rs` covers most paths; usually just adding a row to its provider matrix is enough.

### 7. Documentation

- `README.md`: add the provider to the "Supported Providers" list if it's user-visible.
- `docs/providers.md`: setup instructions, env var, and `settings.json` shape.

## Releasing

Claurst uses a **single workspace version** stamped across every surface. Run `python scripts/bump-version.py vX.Y.Z` from the repo root; it fails loudly if any expected pattern is missing rather than half-stamping a release. It touches:

- `src-rust/Cargo.toml` (`workspace.package.version`)
- `src-rust/Cargo.lock` (the 12 `claurst*` workspace entries)
- `npm/package.json` (`version`)
- `README.md` (shields.io badge text + alt, Beta callout)
- `docs/index.md`, `docs/installation.md`
- `src-rust/crates/acp/registry-template/agent.json` (version + 5 release download URLs)

Versioning is forward-only — the release workflow refuses to ship a tag less than or equal to the highest existing tag. A release is triggered by a `--release` marker in a commit (`.github/workflows/auto-release.yml`); a patch bullet is triggered by a `--patch` marker (`patch-release.yml`). npm publishing runs from `npm-publish.yml` after the GitHub Release exists.

## Issues & PR Comments

When posting issue/PR comments:

- Write the full comment to a temp file and use `gh issue comment --body-file` or `gh pr comment --body-file`.
- Never pass multi-line markdown directly via `--body` in shell commands.
- Preview the exact comment text before posting.
- Post exactly one final comment unless the user explicitly asks for multiple comments.
- If a comment is malformed, delete it immediately, then post one corrected comment.
- Keep comments concise, technical, and in the user's tone.

When creating issues, add labels that map to the relevant crate(s) — for example `crate:tui`, `crate:api`, `crate:tools`, `crate:mcp`, `crate:acp`. If an issue spans multiple crates, add all relevant labels.

When closing issues via commit, include `fixes #<number>` or `closes #<number>` in the commit message — GitHub closes the issue automatically on merge to main.

## **CRITICAL** Git Rules for Parallel Agents

This repo runs parallel agents in worktrees under `.claude/worktrees/`. Multiple agents may be modifying different files in the same checkout simultaneously. You MUST follow these rules:

### Committing

- **ONLY commit files YOU changed in THIS session.** Parallel agents share the checkout, so anything else you stage is someone else's work.
- Commit each piece of work as soon as it is complete and verified. Do not batch commits at the end of a session, and do not wait to be asked. Hold only an intermediate step that would leave the tree broken on its own.
- One commit per logical slice. Keep a behaviour-preserving refactor out of the feature commit that needs it, and keep an incidental fix found mid-task in its own commit.
- ALWAYS include `fixes #<number>` or `closes #<number>` in the commit message when there is a related issue or PR.
- NEVER use `git add -A` or `git add .` — these sweep up changes from other agents.
- ALWAYS use `git add <specific-file-paths>` listing only files you modified.
- Before committing, run `git status` and verify you are only staging YOUR files.
- Track which files you created/modified/deleted during the session.
- Cargo.lock counts as yours if and only if your edits to `Cargo.toml` caused it to change.

### Forbidden Git Operations

These can destroy other agents' work:

- `git reset --hard` — destroys uncommitted changes
- `git checkout .` / `git restore .` — destroys uncommitted changes
- `git clean -fd` — deletes untracked files
- `git stash` — stashes ALL changes including other agents' work
- `git add -A` / `git add .` — stages other agents' uncommitted work
- `git commit --no-verify` — bypasses required checks; never allowed
- Force-push to `main` — never allowed; the patch-release workflow is the only thing that may force-move tags, and it does so via the workflow runner, not from your shell

### Safe Workflow

```bash
# 1. Check status
git status

# 2. Stage only your files
git add src-rust/crates/api/src/providers/foo.rs
git add docs/providers.md

# 3. Commit as soon as this slice is verified
git commit -m "feat(api): add foo provider"

# 4. Push (pull --rebase if needed, but NEVER reset/checkout)
git pull --rebase && git push
```

### If Rebase Conflicts Occur

- Resolve conflicts in YOUR files only.
- If a conflict touches a file you didn't modify, abort the rebase and ask the user.
- NEVER force-push.

### User Override

If the user's instructions conflict with the rules above, ask for confirmation that they want to override the rules. Only then execute their instructions.
