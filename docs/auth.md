# MikMik Authentication Guide

MikMik needs credentials to call the Anthropic API (or another provider's
API). This document covers every supported authentication method, multi-account
account switching, how credentials are stored, how to check and clear them,
and how to authenticate with non-Anthropic providers.

---

## Authentication Methods

MikMik checks for credentials in the following priority order:

1. `--api-key` flag (highest priority, session-only)
2. `api_key` field in `~/.config/mikmik/settings.json`
3. `ANTHROPIC_API_KEY` environment variable
4. The credential for the active Anthropic account in
   `~/.config/mikmik/auth.json`

The first non-empty credential found is used. Provider-specific credentials
(OpenAI, Google, etc.) follow the same pattern but use their own environment
variables and provider config entries.

Codex (OpenAI ChatGPT subscription) accounts work the same way: each login is
an account with its own credential in `auth.json`.

---

## Method 1: API Key

The simplest and most reliable authentication method is a direct API key from
the Anthropic Console.

### Get an API key

1. Log in to [console.anthropic.com](https://console.anthropic.com).
2. Navigate to **Settings > API Keys**.
3. Click **Create Key** and copy the generated `sk-ant-...` key.

### Configure the key

**Option A: Environment variable (recommended)**

Set `ANTHROPIC_API_KEY` in your shell profile. This keeps the key out of any
configuration files that might be committed to version control.

```bash
# Add to ~/.bashrc or ~/.zshrc
export ANTHROPIC_API_KEY="sk-ant-api03-..."
```

On Windows (Command Prompt, permanent):

```cmd
setx ANTHROPIC_API_KEY "sk-ant-api03-..."
```

On Windows (PowerShell profile):

```powershell
$env:ANTHROPIC_API_KEY = "sk-ant-api03-..."
# To persist it:
[System.Environment]::SetEnvironmentVariable("ANTHROPIC_API_KEY","sk-ant-api03-...","User")
```

**Option B: Settings file**

Store the key in `~/.config/mikmik/settings.json`. Ensure the file has restricted
permissions on shared systems.

```json
{
  "config": {
    "api_key": "sk-ant-api03-..."
  }
}
```

**Option C: CLI flag (session-only)**

Pass the key directly for a single run. It is not persisted anywhere.

```bash
mikmik --api-key "sk-ant-api03-..." "your prompt"
```

---

## Method 2: OAuth Login (Browser-based)

MikMik supports an OAuth 2.0 PKCE flow that authenticates through either
the Anthropic Console or Claude.ai in your browser.

> **Important:** The OAuth client IDs in MikMik are registered to Anthropic's
> official Claude Code CLI application. Anthropic's authorization server may
> reject or misattribute OAuth requests originating from MikMik. The API key
> method is the recommended path for MikMik users.
>
> If OAuth login is attempted and fails, use Method 1 (API key) instead.

### Claude.ai flow (default)

```bash
mikmik auth login
```

1. MikMik generates a PKCE code verifier and code challenge. The verifier and
   the CSRF state are 32 bytes from the operating system's random number
   generator. A machine whose OS RNG is unavailable fails the login rather
   than falling back to something weaker.
2. A temporary localhost HTTP server starts on a random port to receive the
   callback.
3. The authorization URL is printed to the terminal and MikMik attempts to
   open it in your default browser.
4. Complete the authorization in the browser (Claude.ai login page).
5. The browser redirects to `http://localhost:<port>/callback` with an
   authorization code.
6. MikMik exchanges the code for tokens via the token endpoint.
7. Tokens are saved in `~/.config/mikmik/auth.json` under the account's name,
   a matching `providers` entry is written to `settings.json`, and that account
   becomes the active one.

This flow produces a Bearer token (`user:inference` scope) used directly for
API calls.

### Console flow (creates an API key)

```bash
mikmik auth login --console
```

This uses the Anthropic Console authorization endpoint. After token exchange,
MikMik calls the Console API to create a new API key, stores it with the
account's credential, and uses it as a standard API key for subsequent requests
rather than as a Bearer token.

### Naming the account

Add `--label <name>` to name the new account (otherwise the name is derived from
the JWT email's local-part). That name is what `mikmik auth switch` takes, and
what `"<account>/<model>"` addresses:

```bash
mikmik auth login --label work
mikmik auth login --label personal
mikmik auth switch personal
```

### Manual fallback

If the browser does not open automatically, MikMik prints the full
authorization URL. Copy and paste it into a browser. After you authorize,
paste the authorization code shown in the browser back into the terminal
when prompted.

---

## Multiple accounts

MikMik stores **multiple named accounts per provider** and lets you switch
between them without re-logging-in. Two vendors keep separate accounts today:
**Anthropic** (Claude.ai / Console) and **Codex** (OpenAI ChatGPT
subscription). Every other provider stores one credential.

This is useful for separating work and personal accounts, juggling
Pro/Max/Team plans, or testing against multiple organizations.

### On-disk layout

An account is two things: a credential in `auth.json`, and an entry in the
`providers` map of `settings.json`. Both are keyed by the account's name.

```
~/.config/mikmik/
├── auth.json          # credentials, keyed by account name
└── settings.json      # the `providers` entry for each account, and the active one
```

`auth.json`:

```json
{
  "credentials": {
    "personal": { "type": "anthropic-oauth", "access_token": "…", "refresh_token": "…" },
    "work":     { "type": "anthropic-oauth", "access_token": "…", "refresh_token": "…" },
    "chatgpt":  { "type": "codex-oauth",     "access_token": "…", "refresh_token": "…" },
    "openai":   { "type": "api",             "key": "sk-…" }
  }
}
```

The `type` field says which wire protocol the credential belongs to: `api` for a
plain key, `oauth` for a device-flow token, `anthropic-oauth`, or `codex-oauth`.

`settings.json` carries the matching `providers` entry, with `protocol` naming
the vendor when the account is not named after it:

```json
{
  "provider": "personal",
  "providers": {
    "personal": { "enabled": true, "protocol": "anthropic" },
    "work":     { "enabled": true, "protocol": "anthropic" },
    "chatgpt":  { "enabled": true, "protocol": "codex" }
  }
}
```

Because an account has a name of its own, `"<account>/<model>"` addresses it
directly: `anthropic:personal/sonnet` for the advisor, `work/claude-opus-4-6`
in the model picker.

Account names are slugified: lower-cased, with anything outside `[a-z0-9_-]`
replaced by a dash, and a `-2`, `-3` suffix when the name is taken.

### Migration from the old layout

Accounts used to live in an `accounts.json` registry plus one token file per
profile under `accounts/<provider>/<id>/`. That is read once, at startup, and
folded into the two files above. The old registry and directory are moved to
`accounts-backup-<timestamp>/` rather than deleted, and the run says what it
moved.

Two older layouts are migrated the same way:
`~/.config/mikmik/oauth_tokens.json` and `~/.config/mikmik/codex_tokens.json`.

### CLI

`mikmik auth` and `mikmik codex` are symmetric — same subcommands for both
providers:

```bash
# Add accounts (each login becomes its own account)
mikmik auth login                       # Claude.ai (default)
mikmik auth login --console             # Console / API-key flow
mikmik auth login --label work          # name the account
mikmik codex login                      # ChatGPT/Codex OAuth
mikmik codex login --label personal

# Inspect
mikmik auth status                      # show the active Anthropic account
mikmik auth list                        # every Anthropic account
mikmik codex list                       # every Codex account
mikmik accounts                         # both at once (use --json for JSON)

# Switch the active account
mikmik auth switch work
mikmik codex switch personal

# Remove a stored account
mikmik auth remove work                 # delete the credential and its providers entry
mikmik codex remove personal

# Logout (clears the active account's credential)
mikmik auth logout
mikmik codex logout
```

`mikmik auth status` and `mikmik codex status` exit `0` when logged in and
`1` otherwise, so they can drive scripts:

```bash
if mikmik codex status > /dev/null; then
  echo "Codex login present"
fi
```

### Slash commands

Inside the interactive REPL the same operations are available as slash
commands — Anthropic is the default, pass `--codex` to target Codex:

```
/login                          # OAuth login (Claude.ai)
/login --console                # API-key flow
/login --codex                  # add a Codex account
/login --label work             # name the new account
/logout                         # clear active Anthropic credentials
/logout --codex                 # clear active Codex credentials
/logout --all                   # purge every stored Anthropic account
/accounts                       # list every stored account
/switch personal                # set active Anthropic to "personal"
/switch --codex work            # set active Codex to "work"
```

`/accounts` lists every account with a `*` next to the active one and shows
email and subscription tier when known.

### Identity detection

When you log in, MikMik decodes the JWT id_token (or access token for Codex)
to extract your email and provider-side account_id. If a stored account already
matches that identity, its credential is refreshed instead of a duplicate being
created, so re-logging-in the same account is idempotent.

## Method 3: Device Code Flow

The device code flow (RFC 8628) is designed for headless or server
environments where opening a browser is not practical. Currently this flow
is used internally for GitHub Copilot authentication.

For headless environments without a Copilot subscription, the API key method
(Method 1) is the recommended approach. Set `ANTHROPIC_API_KEY` in the
environment before running MikMik in a CI/CD or server context.

```bash
# Headless / CI example
ANTHROPIC_API_KEY="sk-ant-..." mikmik --print "summarize the last 10 commits"
```

---

## Token Storage

Every credential lives in one file:

```
~/.config/mikmik/auth.json
```

It is keyed by account name, and each entry names its own type:

```json
{
  "credentials": {
    "openai": { "type": "api", "key": "sk-..." },
    "github-copilot": {
      "type": "oauth",
      "access": "...",
      "refresh": "...",
      "expires": 1700000000
    },
    "personal": {
      "type": "anthropic-oauth",
      "access_token": "...",
      "refresh_token": "...",
      "expires_at_ms": 1700000000000,
      "scopes": ["user:inference", "user:profile"],
      "email": "you@example.com",
      "api_key": "sk-ant-..."
    },
    "chatgpt": { "type": "codex-oauth", "access_token": "...", "refresh_token": "..." }
  }
}
```

An Anthropic credential's scope list is what decides whether it is used as a
Bearer token or as a minted API key.

The file is written with user-only permissions (`600` on Unix). Do not commit
it to version control.

---

## Checking Authentication Status

```bash
mikmik auth status
```

Prints a human-readable summary:

```
Logged in.
  API provider: Anthropic
  Login method: API Key
  Billing mode: API
  Key source:   ANTHROPIC_API_KEY
```

For machine-readable output:

```bash
mikmik auth status --json
```

Example JSON output:

```json
{
  "loggedIn": true,
  "authMethod": "api_key",
  "apiProvider": "Anthropic",
  "billing": "API",
  "apiKeySource": "ANTHROPIC_API_KEY"
}
```

The exit code is `0` when logged in, `1` when not logged in. This makes
`auth status` suitable for scripting:

```bash
if mikmik auth status > /dev/null 2>&1; then
  echo "credentials present"
fi
```

---

## Logging Out

By default, `logout` removes the **active** account's credential; other stored
accounts are untouched, so a stored secondary account becomes the candidate for
next selection.

```bash
# Remove the active Anthropic account
mikmik auth logout

# Remove the active Codex account
mikmik codex logout

# Or from inside the REPL
/logout
/logout --codex
```

To purge every stored account for a provider (and clear any API key in
`settings.json`):

```
/logout --all          # Anthropic
/logout --codex --all  # Codex
```

API keys set via environment variables are not affected by `logout`; remove
them from your shell profile manually.

To delete a specific stored account without making it active first:

```bash
mikmik auth remove work
mikmik codex remove personal
```

---

## Token Refresh

When MikMik loads OAuth tokens for the active profile and the access token
is expired, it automatically attempts a silent refresh:

1. A `POST` request is sent to the provider's token endpoint with the stored
   refresh token.
2. If successful, the new access token (and optionally a new refresh token)
   is written back to the same per-profile token file.
3. The refreshed token is used for the current session.

If the refresh fails (network error, expired refresh token, revoked grant),
MikMik falls back to any configured API key. If no API key is available,
authentication fails and you must run `mikmik auth login` again, optionally with
`--label <name>` to reuse an account name.

---

## Multiple Providers

MikMik supports simultaneous configuration of credentials for multiple
providers. Each provider looks for credentials in this order:

1. `api_key` in the provider's entry under `providers` in `settings.json`
2. The provider-specific environment variable (see table below)
3. The credential stored in `~/.config/mikmik/auth.json` under that account's name

### Provider environment variables

| Provider         | Environment variable |
|------------------|----------------------|
| `anthropic`      | `ANTHROPIC_API_KEY`  |
| `openai`         | `OPENAI_API_KEY`     |
| `google`         | `GOOGLE_API_KEY`     |
| `groq`           | `GROQ_API_KEY`       |
| `cerebras`       | `CEREBRAS_API_KEY`   |
| `deepseek`       | `DEEPSEEK_API_KEY`   |
| `mistral`        | `MISTRAL_API_KEY`    |
| `xai`            | `XAI_API_KEY`        |
| `openrouter`     | `OPENROUTER_API_KEY` |
| `togetherai`     | `TOGETHER_API_KEY`   |
| `perplexity`     | `PERPLEXITY_API_KEY` |
| `cohere`         | `COHERE_API_KEY`     |
| `deepinfra`      | `DEEPINFRA_API_KEY`  |
| `venice`         | `VENICE_API_KEY`     |
| `github-copilot` | `GITHUB_TOKEN`       |
| `azure`          | `AZURE_API_KEY`      |
| `huggingface`    | `HF_TOKEN`           |
| `nvidia`         | `NVIDIA_API_KEY`     |

### Example: multiple providers in settings.json

```json
{
  "providers": {
    "anthropic": {
      "api_key": null,
      "enabled": true
    },
    "openai": {
      "api_key": "sk-...",
      "enabled": true
    },
    "ollama": {
      "api_base": "http://localhost:11434",
      "enabled": true
    },
    "openrouter": {
      "api_key": "sk-or-...",
      "enabled": true,
      "models_whitelist": ["anthropic/claude-sonnet-4", "openai/gpt-4o"]
    }
  }
}
```

Switch providers at runtime:

```bash
# Use OpenAI for this session
mikmik --provider openai --model gpt-4o "your prompt"

# Use a local Ollama model (no API key needed)
mikmik --provider ollama --model llama3.2 "your prompt"

# Or via environment variable
MIKMIK_PROVIDER=google mikmik "your prompt"
```

---

## Local Models (No API Key)

Providers that run locally require no API key:

**Ollama:**

```bash
# Install Ollama from https://ollama.ai and pull a model
ollama pull llama3.2

# Run MikMik against it
mikmik --provider ollama --model llama3.2
```

**LM Studio:**

```bash
# Start the LM Studio local server (default port 1234)
mikmik --provider lmstudio
```

**llama.cpp server:**

```bash
mikmik --provider llamacpp --api-base http://localhost:8080
```

---

## Security Recommendations

- Store API keys in environment variables or a secrets manager rather than in
  `settings.json`, especially on shared or CI systems.
- Restrict permissions on `~/.config/mikmik/` to your user only:
  ```bash
  chmod 700 ~/.config/mikmik
  chmod 600 ~/.config/mikmik/auth.json
  chmod 600 ~/.config/mikmik/settings.json
  ```
  MikMik already sets `0600` on `auth.json` on Unix; the command above also
  covers `settings.json`, which holds a key when one was written there.
- Do not commit `~/.config/mikmik/` to version control.
- Add `.mikmik/` to your project's `.gitignore` to prevent accidentally
  committing project-level settings files that may contain keys.
- Rotate API keys periodically from the Anthropic Console.
- Use `mikmik auth logout` on shared machines before logging out of your
  user session.
