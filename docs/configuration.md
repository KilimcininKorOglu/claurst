# Claurst Configuration Reference

Claurst is configured through a layered system of JSON files, environment
variables, and command-line flags. This document describes every option.

---

## Configuration File Location

The global settings file lives at:

```
~/.claurst/settings.json
```

The directory `~/.claurst/` is created automatically on first run if it does
not exist. The file is standard JSON (or JSONC — comments are stripped before
parsing).

### Per-project settings

Claurst walks up from the current working directory looking for a project-level
settings file. The first file found wins (project settings take precedence over
global settings):

```
<project-root>/.claurst/settings.json
<project-root>/.claurst/settings.jsonc
```

Settings that appear in the project file override the corresponding global
values. Keys absent from the project file fall back to the global value.

---

## Top-level Settings Structure

```json
{
  "version": 1,
  "provider": "anthropic",
  "config": { ... },
  "providers": { ... },
  "modelOverrides": { ... },
  "projects": { ... },
  "commands": { ... },
  "formatter": { ... },
  "agents": { ... },
  "skills": { ... },
  "permissionRules": [],
  "enabledPlugins": [],
  "disabledPlugins": [],
  "hasCompletedOnboarding": false,
  "showMessageTimestamps": false,
  "advisorModel": "claude-opus-4-6",
  "companion": { ... },
  "remoteControl": { ... },
  "acpAgents": { ... }
}
```

Most day-to-day options live inside the `config` object. Provider credentials
live in the `providers` map. Corrected model metadata for self-hosted or
unknown models lives in the `modelOverrides` map — see
[Model metadata overrides](providers.md#overriding-model-metadata).

### Plugin selection

| Key               | Type             | Default | Description                                                              |
|-------------------|------------------|---------|--------------------------------------------------------------------------|
| `enabledPlugins`  | array of strings | []      | Names `/plugin enable` has recorded. Discovery already loads every plugin it finds, so this list only cancels a previous `disable`. |
| `disabledPlugins` | array of strings | []      | Plugin names to skip. A listed plugin contributes no commands, hooks, skills, agents, or MCP servers. |
| `pluginConfig`    | object           | {}      | Values for the options a plugin declares under `userConfig`, keyed by plugin name then option name. Edited in `/settings`; the plugin reads them from `CLAUDE_PLUGIN_CONFIG`. See [Plugins](plugins.md#user_config). |

`/plugin enable <name>` and `/plugin disable <name>` write these lists. The
running session keeps the plugin set it loaded at startup until `/plugin
reload` rereads the directories and applies the change. A name in
`disabledPlugins` that matches no
discovered plugin is ignored. `claurst --bare` skips plugin discovery
entirely, regardless of both lists.

### Skills

| Key            | Type             | Default | Description                                                            |
|----------------|------------------|---------|------------------------------------------------------------------------|
| `skills.paths` | array of strings | []      | Extra directories to search for skills. A relative path resolves against the working directory. |
| `skills.urls`  | array of strings | []      | Git repository URLs to fetch skills from. Each is cloned once and then cached. |

A skill is a prompt template. Discovery reads two layouts in every searched
directory: a flat `<name>.md` file, and a `<name>/SKILL.md` package, which
takes its name from the directory unless the frontmatter sets `name:`. The
searched directories are `.claurst/skills/` and `.agents/skills/` walking up
from the working directory, then `<claurst home>/skills/`, then `skills.paths`,
then `skills.urls`. Each installed plugin's `skills/` directory is added to the
search at startup. Run a skill by its name as a slash command, and list them
all with `/skills`.

### Transcript display

| Key                     | Type    | Default | Description                                                                                                                                                                    |
|-------------------------|---------|---------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `showMessageTimestamps` | boolean | false   | Print the local time beneath each message. Times are stored in UTC and converted using the machine's time zone. Messages from an earlier day also show their date (`13 Aug 14:32`). |

Toggle it from the TUI with `/config` → **Show message timestamps**. Turns
restored from a transcript recorded before this option existed carry no time
and render without one.

### Advisor

| Key            | Type   | Default | Description                                                                                                                                       |
|----------------|--------|---------|-------------------------------------------------------------------------------------------------------------------------------------------------------|
| `advisorModel` | string | unset   | Second model consulted for a review. A bare ID runs against the active provider; `provider/model` targets a specific one; `provider:account/model` also targets a specific stored login. Unset disables the advisor. |

Set it with [`/advisor <model>`](commands.md#advisor) rather than by hand. When
unset, the `Advisor` tool is not offered to the model at all.

### Companion

The small creature beside the input box. See [`/buddy`](commands.md#buddy).

| Key       | Type           | Default | Description                                                                                             |
|-----------|----------------|---------|-----------------------------------------------------------------------------------------------------------|
| `enabled` | boolean        | false   | Show the companion and describe it to the model. Off by default: on, it costs a model call to hatch and a block in every system prompt. |
| `model`   | string \| null | unset   | Model that hatches the companion and writes its replies. Unset uses the session model.                  |

```json
"companion": {
  "enabled": true,
  "model": "claude-haiku-4-5-20251001"
}
```

Toggle it with `/buddy on` / `/buddy off`, or from `/config` → **Companion**. The generated name and personality live in `companion.json` beside this file, not here; the body is never stored, because it is re-derived from your identity on every read.

### Remote control

Points the bridge at a relay you host yourself, so a phone or browser can drive a running session. See [Remote Control](remote-control) for the full setup.

There is no separate remote permission policy. `config.permission_mode` decides whether a tool asks at all; once it asks, the answer may come from the terminal or the remote client.

| Key             | Type           | Default | Description                                                                                                       |
|-----------------|----------------|---------|-------------------------------------------------------------------------------------------------------------------|
| `url`           | string         | unset   | Base address of your relay, for example `https://relay.example`. A trailing slash is trimmed.                     |
| `token`         | string         | unset   | Shared secret, at least 32 characters. Shorter values are refused and the bridge does not start.                  |
| `label`         | string \| null | unset   | Name shown in the session list. Falls back to the machine's hostname.                                             |

```json
"remoteControl": {
  "url": "https://relay.example",
  "token": "a-generated-token-of-at-least-32-characters",
  "label": "workstation"
}
```

This block is read from the user settings file only. A project settings file cannot set it, because pointing the bridge at a relay is a decision about the machine, not about the repository.

`CLAURST_BRIDGE_URL` and `CLAURST_BRIDGE_TOKEN` override it when set.

### External ACP agents

Agents that speak the [Agent Client Protocol](https://agentclientprotocol.com/), reachable through the `AcpAgent` tool. Keys are the names the model uses to pick one.

| Key       | Type              | Default  | Description                                                                                  |
|-----------|-------------------|----------|----------------------------------------------------------------------------------------------|
| `command` | string            | required | Executable to run.                                                                            |
| `args`    | string[]          | `[]`     | Arguments passed to it, usually whatever puts the agent in ACP mode.                          |
| `env`     | object            | `{}`     | Extra environment for the subprocess. Values go through `{env:VARNAME}` substitution.        |

```json
"acpAgents": {
  "cursor": {
    "command": "agent",
    "args": ["--force", "acp"]
  },
  "gemini": {
    "command": "gemini",
    "args": ["--experimental-acp"],
    "env": { "GEMINI_API_KEY": "{env:GEMINI_API_KEY}" }
  }
}
```

The tool is only offered to the model when this block names at least one agent. Everything the sub-agent asks to do is approved through the same permission prompt as a local tool. See [Tools](tools#acpagenttool) for the full behaviour.

This block is read from the user settings file only. An agent definition names an executable the model can invoke, so a repository able to add one would gain arbitrary code execution on your machine.

---

## The `config` Object

The `config` object holds runtime behaviour options.

### Model and token settings

| Key          | Type            | Default          | Description                                                                                                                          |
|--------------|-----------------|------------------|--------------------------------------------------------------------------------------------------------------------------------------|
| `api_key`    | string \| null  | null             | Anthropic API key. Overrides `ANTHROPIC_API_KEY` env var. Prefer the env var in shared environments.                                 |
| `model`      | string \| null  | provider default | Model ID to use. When absent, the provider's default is used (e.g. `claude-sonnet-4-6` for Anthropic, `gpt-4o` for OpenAI).          |
| `max_tokens` | integer \| null | 8192             | Maximum tokens per model response.                                                                                                    |
| `provider`   | string \| null  | `"anthropic"`    | Active provider. See the [Providers](#providers) section.                                                                             |
| `effort`     | string \| null  | unset            | Reasoning effort a session starts at: `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultracode`. Unset leaves it to the turn. |

### Permission mode

| Key               | Type   | Default     | Description                                                                                                       |
|-------------------|--------|-------------|-------------------------------------------------------------------------------------------------------------------|
| `permission_mode` | string | `"default"` | Controls how tool permissions are enforced. One of `"default"`, `"acceptEdits"`, `"bypassPermissions"`, `"plan"`. |

See [Permission Modes](#permission-modes) for a full description of each value.

### Interface and output

| Key             | Type           | Default     | Description                                                                                                                                                 |
|-----------------|----------------|-------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `theme`         | string         | `"default"` | Color theme for the TUI. One of `"default"`, `"dark"`, `"light"`, `"deuteranopia"`.                                                                         |
| `output_style`  | string \| null | null        | Named output style. Built-in values: `"default"`, `"concise"`, `"verbose"`. Custom styles can be added as Markdown files under `~/.claurst/output-styles/`. |
| `output_format` | string         | `"text"`    | Output format for headless (`--print`) mode. One of `"text"`, `"json"`, `"stream-json"`.                                                                    |
| `verbose`       | boolean        | false       | Enable debug-level log output.                                                                                                                              |

### Context compaction

| Key                 | Type    | Default | Description                                                                            |
|---------------------|---------|---------|----------------------------------------------------------------------------------------|
| `auto_compact`      | boolean | true    | Automatically compact the conversation context when the context window nears capacity. |
| `compact_threshold` | float   | 0.85    | Fraction of the context window that triggers auto-compaction (0.0–1.0).                |

### Turn behaviour

| Key                  | Type    | Default | Description                                                                                                                                                                 |
|----------------------|---------|---------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `degradationSummary` | boolean | true    | Run one final tool-less turn asking the model to summarise its progress when the turn limit is reached. Set to `false` to stop at the limit and take the last message instead. |
| `autoPoke`           | boolean | true    | Append a reminder about incomplete todos to the system prompt after the second turn. Set to `false` when the todo list is a record rather than a work queue.                   |

Both save one request per run when switched off. Leaving a key unset is the same
as `true`, so upgrading changes nothing.

The turn limit itself comes from `--max-turns` or from an agent definition's
`max_turns`.

### System prompt

| Key                    | Type           | Default | Description                                                                           |
|------------------------|----------------|---------|---------------------------------------------------------------------------------------|
| `custom_system_prompt` | string \| null | null    | Replace the default Claurst system prompt entirely with this text.                    |
| `append_system_prompt` | string \| null | null    | Append this text to the end of the assembled system prompt (after AGENTS.md content). |

The same two can be set per run from the command line, which overrides the settings file:

| Flag                              | Effect                                                              |
|-----------------------------------|---------------------------------------------------------------------|
| `--system-prompt <TEXT>`, `-s`    | Replace the base prompt with `TEXT`.                                |
| `--system-prompt-file <PATH>`     | Replace the base prompt with the file's contents. Fails if unreadable. |
| `--append-system-prompt <TEXT>`   | Append `TEXT` after the assembled prompt.                            |

`--system-prompt` and `--system-prompt-file` are mutually exclusive. Run `claurst --dump-system-prompt` with the same flags to see exactly what a run would send.

### Tool access

| Key                | Type             | Default  | Description                                                                                |
|--------------------|------------------|----------|--------------------------------------------------------------------------------------------|
| `allowed_tools`    | array of strings | [] (all) | Restrict the tool set to this explicit list. An empty array means all tools are available. |
| `disallowed_tools` | array of strings | []       | Always deny these tools, regardless of other settings.                                     |

Tool names match the internal names: `Bash`, `Read`, `Write`, `Edit`, `Glob`,
`Grep`, `WebSearch`, `WebFetch`, `TodoWrite`, `TodoRead`, and MCP tool names
prefixed with their server name (`myserver_toolname`).

### Tool behaviour

| Key                   | Type    | Default | Description                                                                                                                                                                          |
|-----------------------|---------|---------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `includeIgnoredFiles` | boolean         | false   | Let `Glob` and `Grep` search files that `.gitignore` and `.ignore` exclude. Off by default, so a build directory does not drown the results.                                     |
| `searxngUrl`          | string \| unset | unset   | Base address of the SearXNG instance `WebSearch` prefers, for example `http://localhost:8080`. Overrides the `SEARXNG_URL` environment variable. Unset means no instance.        |
| `webSearchFallback`   | boolean         | false   | Let `WebSearch` continue with Brave or DuckDuckGo when the SearXNG instance is unreachable. Off by default, so a query aimed at a private instance stays there.                  |

All three are editable from `/settings`. Turning **SearXNG** on there prompts for
the address and writes it to `searxngUrl`; turning it off clears the key.

### Interface

| Key               | Type    | Default | Description                                                                                                                                                  |
|-------------------|---------|---------|--------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `timelineEnabled` | boolean | false   | Record every tool call and finished turn, and offer the panel through `/timeline` and `Ctrl+Shift+L`. Off by default; while off nothing is collected at all. |
| `mouseCapture`    | boolean | true    | Let Claurst handle the mouse: wheel scrolling, right-click menus and drag-select. Turn it off to give the mouse back to the terminal. Applies at next start. |

Both are editable from `/settings`, as **Execution timeline** and **Mouse
capture**. See [Commands](commands.md#timeline) for what the timeline panel
shows.

While Claurst captures the mouse, the terminal no longer sees it, so its own
selection and its right-click paste stop working. That matters most over SSH,
where the remote host often has no `wl-copy` or `xclip` for `Ctrl+V` to reach:
set `mouseCapture` to `false` and paste with the terminal's own shortcut
instead. `Shift+Insert` works either way.

Copying out needs nothing installed. When no clipboard tool answers, Claurst
hands the text to the terminal emulator with OSC 52, which works over SSH.

### Status line

`statusLine` runs a shell command and shows its output in its own rows directly
above the footer, so you can keep context usage, cost or git state permanently
in view.

```json
{
  "config": {
    "statusLine": {
      "type": "command",
      "command": "~/.claurst/statusline.sh",
      "padding": 2,
      "refreshInterval": 5
    }
  }
}
```

| Key                    | Type    | Default | Description                                                                                                     |
|------------------------|---------|---------|-------------------------------------------------------------------------------------------------------------------|
| `type`                 | string  | command | Only `"command"` runs anything. Any other value leaves the status line off.                                       |
| `command`              | string  | —       | Runs in a shell, so a script path and an inline pipeline both work.                                              |
| `padding`              | number  | 0       | Extra columns of indentation on each side of the output.                                                          |
| `refreshInterval`      | number  | —       | Re-run every N seconds on top of the state-driven updates. Minimum 1. Leave it out to run only when state changes. |
| `hideVimModeIndicator` | boolean | false   | Suppress the built-in `-- INSERT --` line, for a status line that prints `vim.mode` itself.                        |

**Only your own global settings file can set this.** A project's
`.claurst/settings.json` is ignored for `statusLine`, in whole: it can neither
replace the command nor introduce one. Without that rule, cloning a repository
would run whatever shell command the repository asked for.

The command runs when the session changes: at startup, when a reply arrives,
when the model, directory, permission mode, vim mode, output style, effort
level, context usage or cost move. Bursts collapse into a single run, a run
still going when the next change lands is killed, and an idle session with no
`refreshInterval` runs nothing at all. Output is capped at 4 KB and a command
that has neither finished nor printed within 10 seconds is abandoned.

Output may span several lines, and each is shown on its own row, up to half the
terminal height. ANSI colour is rendered rather than printed; OSC 8 hyperlinks
show their label as plain text. `COLUMNS` and `LINES` carry the terminal size,
which a script cannot measure for itself because its output is captured.

The session arrives as JSON on stdin:

```json
{
  "session_id": "…",
  "transcript_path": "~/.claurst/projects/…/….jsonl",
  "version": "0.1.7",
  "cwd": "/work/project",
  "workspace": { "current_dir": "/work/project", "project_dir": "/work" },
  "model": { "id": "claude-opus-5", "display_name": "claude-opus-5" },
  "permission_mode": "Default",
  "output_style": { "name": "auto" },
  "effort": { "level": "high" },
  "vim": { "mode": "NORMAL" },
  "cost": { "total_cost_usd": 1.25, "total_duration_ms": 61000 },
  "context_window": {
    "total_input_tokens": 1500,
    "total_output_tokens": 500,
    "context_window_size": 200000,
    "used_percentage": 20.0,
    "remaining_percentage": 80.0,
    "current_usage": {
      "input_tokens": 1000,
      "output_tokens": 500,
      "cache_creation_input_tokens": 200,
      "cache_read_input_tokens": 300
    }
  },
  "exceeds_200k_tokens": false
}
```

`vim` is absent unless vim mode is on, and `transcript_path` is absent when the
session has no file yet. A script that reads the JSON with `jq` looks like this:

```bash
#!/bin/bash
input=$(cat)
model=$(printf '%s' "$input" | jq -r '.model.display_name')
dir=$(printf '%s' "$input" | jq -r '.workspace.current_dir')
pct=$(printf '%s' "$input" | jq -r '.context_window.used_percentage // 0' | cut -d. -f1)
printf '\033[32m[%s]\033[0m %s | %s%% context\n' "$model" "${dir##*/}" "$pct"
```

`/statusline` reports the configured command alongside the built-in status bar
items; see [Commands](commands.md).

### Directory access

| Key               | Type             | Default | Description                                                                                                              |
|-------------------|------------------|---------|--------------------------------------------------------------------------------------------------------------------------|
| `additional_dirs` | array of strings | []      | Additional filesystem paths Claurst is allowed to read and write. Equivalent to passing `--add-dir` on the command line. Each one becomes a named workspace root the model can address as `&root-name/path`; see [`--add-dir`](advanced.md#--add-dir). |

### MCP servers

| Key           | Type                       | Default | Description                                           |
|---------------|----------------------------|---------|-------------------------------------------------------|
| `mcp_servers` | array of `McpServerConfig` | []      | Model Context Protocol servers to connect at startup. |

Each `McpServerConfig` object:

```json
{
  "name": "my-server",
  "command": "/path/to/server",
  "args": ["--flag"],
  "env": { "MY_VAR": "value" },
  "type": "stdio"
}
```

`type` can be `"stdio"` (default) or `"http"` (for HTTP-SSE servers, in which
case `command` is the base URL).

Servers declared by an installed plugin join this list at startup. A server
that came with the project (from `<project>/.claurst/settings.json` or a plugin
under `<project>/.claurst/plugins/`) needs approval before it launches; see
[Plugins](plugins.md#mcp_servers).

### Environment variables injected into tools

| Key   | Type                     | Default | Description                                                                                                                                                                                                         |
|-------|--------------------------|---------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `env` | object (string → string) | {}      | Environment variables injected into every tool execution. Useful for setting project-specific tokens without polluting the system environment. Values may reference existing env vars using `{env:VARNAME}` syntax. |

### Hooks

Hooks let you run shell commands in response to lifecycle events. They are
defined as a map from event name to an array of hook entries.

```json
"hooks": {
  "PreToolUse": [
    { "command": "echo tool=$TOOL_NAME", "blocking": false }
  ],
  "PostToolUse": [
    { "command": "/path/to/my-logger.sh", "tool_filter": "Bash", "blocking": false }
  ],
  "Stop": [
    { "command": "notify-send 'Claurst done'", "blocking": false }
  ]
}
```

Available events:

| Event              | When it fires                                              |
|--------------------|------------------------------------------------------------|
| `PreToolUse`       | Before a tool executes. Receives event JSON on stdin.      |
| `PostToolUse`      | After a tool returns its result.                           |
| `Stop`             | When the model finishes its turn (stop reason).            |
| `PostModelTurn`    | After the model samples a response, before tool execution. |
| `UserPromptSubmit` | When the user submits a prompt.                            |
| `Notification`     | General-purpose notification event.                        |

Hook entry fields:

| Field         | Type           | Description                                                         |
|---------------|----------------|---------------------------------------------------------------------|
| `command`     | string         | Shell command to execute.                                           |
| `tool_filter` | string \| null | Only run for this tool name (`PreToolUse`/`PostToolUse` only).      |
| `blocking`    | boolean        | If true, a non-zero exit code blocks the operation. Default: false. |

---

## Permission Modes

The `permission_mode` field (and `--permission-mode` CLI flag) controls how
tool calls are approved.

### `default`

Read-only operations (file reads, searches, glob) are permitted automatically.
Write and execute operations (file writes, shell commands) prompt the user for
confirmation in the TUI, or are denied in headless mode.

### `acceptEdits`

All tool calls — reads, writes, and shell commands — are automatically
accepted without prompting. This is useful for trusted automation pipelines
where you want maximum throughput.

### `bypassPermissions`

All permission checks are skipped entirely. Every tool call is allowed
unconditionally. This mode cannot be used when running as root or via `sudo`
on Unix systems (Claurst blocks it).

Use with caution: the model can read and modify any file reachable from the
current working directory without any user confirmation.

### `plan`

Read-only mode. File reads and searches are allowed; file writes and command
execution are blocked. This matches the built-in `plan` agent's behaviour and
is useful for code analysis sessions where you want to prevent accidental
modifications.

The permission mode can also be overridden per-session on the command line:

```bash
claurst --permission-mode acceptEdits "refactor the auth module"
claurst --dangerously-skip-permissions "..."  # equivalent to bypassPermissions
```

---

## AGENTS.md Memory Files

AGENTS.md files are plain Markdown documents that Claurst injects into the
system prompt at startup. They let you give the model persistent context about
your project, coding standards, or personal preferences without repeating
yourself in every session.

### File locations and priority

Claurst loads AGENTS.md files from four locations. They are processed in the
following order (earlier = higher priority, later content is appended below):

| Scope   | Path                                | Description                                                                                                |
|---------|-------------------------------------|------------------------------------------------------------------------------------------------------------|
| Managed | `~/.claurst/rules/*.md`             | Global policy files. All `.md` files in this directory are loaded in alphabetical order.                   |
| User    | `~/.claurst/AGENTS.md`              | Your personal preferences and instructions, applied to all projects.                                       |
| Project | `<project-root>/AGENTS.md`          | Project-level context: architecture notes, conventions, workflows. Typically committed to version control. |
| Local   | `<project-root>/.claurst/AGENTS.md` | Local overrides not committed to version control (add `.claurst/` to `.gitignore`).                        |

Files from all four locations are concatenated (separated by blank lines) into
a single system-prompt fragment. If the same instruction appears at multiple
levels, the narrower scope (Project/Local) effectively wins because it appears
later in the prompt.

### CLAUDE.md compatibility

Files named `CLAUDE.md` in the same locations are treated identically to
`AGENTS.md`. Both names are supported for compatibility with the TypeScript
Claude Code CLI.

### YAML frontmatter

AGENTS.md files may begin with optional YAML frontmatter to control loading:

```markdown
---
memory_type: project
priority: 10
scope: project
---

# My Project Notes

Always use 4-space indentation. Prefer `anyhow` for error handling.
```

Frontmatter fields:

| Field         | Description                                                                      |
|---------------|----------------------------------------------------------------------------------|
| `memory_type` | Informal label (currently informational only).                                   |
| `priority`    | Integer sort priority (lower numbers are prepended first within the same scope). |
| `scope`       | Informational label for documentation purposes.                                  |

### @include directives

AGENTS.md files support `@include` to pull in content from other files:

```markdown
# Project Guide

@include ./docs/architecture.md
@include ~/shared-notes/coding-standards.md
```

Paths may be relative to the including file, absolute, or tilde-expanded.
Circular includes are detected and skipped. Files larger than 40 KB are
skipped with a warning comment.

### Disabling AGENTS.md loading

To skip all AGENTS.md files for a session:

```bash
claurst --no-claude-md "your prompt"
```

Or in a session, use the `--bare` flag to disable AGENTS.md, hooks, and
plugins simultaneously.

---

## Providers

Claurst can send requests to multiple LLM providers. Set the active provider
via the `provider` key in settings or the `--provider` CLI flag.

### Provider IDs

| Provider ID      | Default model                             |
|------------------|-------------------------------------------|
| `anthropic`      | `claude-sonnet-4-6` (or latest)           |
| `openai`         | `gpt-4o`                                  |
| `google`         | `gemini-2.5-flash`                        |
| `groq`           | `llama-3.3-70b-versatile`                 |
| `cerebras`       | `llama-3.3-70b`                           |
| `deepseek`       | `deepseek-chat`                           |
| `mistral`        | `mistral-large-latest`                    |
| `xai`            | `grok-2`                                  |
| `openrouter`     | `anthropic/claude-sonnet-4`               |
| `togetherai`     | `meta-llama/Llama-3.3-70B-Instruct-Turbo` |
| `perplexity`     | `sonar-pro`                               |
| `cohere`         | `command-r-plus`                          |
| `deepinfra`      | `meta-llama/Llama-3.3-70B-Instruct`       |
| `github-copilot` | `gpt-4o`                                  |
| `ollama`         | `llama3.2`                                |
| `lmstudio`       | `default`                                 |
| `llamacpp`       | `default`                                 |
| `azure`          | `gpt-4o`                                  |
| `amazon-bedrock` | `anthropic.claude-sonnet-4-6-v1`          |
| `venice`         | `llama-3.3-70b`                           |

### Per-provider configuration

Each provider can have its own entry in the `providers` map (top-level in
`settings.json`) or in `config.provider_configs`. Provider-level `api_key`
and `api_base` override the corresponding environment variables.

```json
"providers": {
  "anthropic": {
    "api_key": "sk-ant-...",
    "api_base": "https://api.anthropic.com",
    "enabled": true,
    "models_whitelist": [],
    "models_blacklist": []
  },
  "openai": {
    "api_key": "sk-...",
    "enabled": true
  },
  "ollama": {
    "api_base": "http://localhost:11434",
    "enabled": true
  }
}
```

`ProviderConfig` fields:

| Field              | Type           | Description                                     |
|--------------------|----------------|-------------------------------------------------|
| `api_key`          | string \| null | API key for this provider.                      |
| `api_base`         | string \| null | Override the default API base URL.              |
| `enabled`          | boolean        | Whether this provider is active. Default: true. |
| `models_whitelist` | array          | If non-empty, only these model IDs are offered. |
| `models_blacklist` | array          | These model IDs are never offered.              |
| `options`          | object         | Provider-specific passthrough options.          |

---

## Environment Variables

| Variable               | Description                                                                     |
|------------------------|---------------------------------------------------------------------------------|
| `ANTHROPIC_API_KEY`    | Anthropic API key. Checked after the `config.api_key` setting.                  |
| `ANTHROPIC_BASE_URL`   | Override the Anthropic API base URL.                                            |
| `CLAURST_PROVIDER`     | Active provider. Equivalent to `--provider`.                                    |
| `CLAURST_API_BASE`     | Override the API base URL for the active provider. Equivalent to `--api_base`.  |
| `CLAURST_GOALS`        | Set to `0` to disable the goal system (`/goal` command and `GoalCompleteTool`). |
| `OPENAI_API_KEY`       | API key for the `openai` provider.                                              |
| `GOOGLE_API_KEY`       | API key for the `google` provider.                                              |
| `GROQ_API_KEY`         | API key for the `groq` provider.                                                |
| `XAI_API_KEY`          | API key for the `xai` provider.                                                 |
| `MISTRAL_API_KEY`      | API key for the `mistral` provider.                                             |
| `OPENROUTER_API_KEY`   | API key for the `openrouter` provider.                                          |
| `DEEPSEEK_API_KEY`     | API key for the `deepseek` provider.                                            |
| `COHERE_API_KEY`       | API key for the `cohere` provider.                                              |
| `DEEPINFRA_API_KEY`    | API key for the `deepinfra` provider.                                           |
| `VENICE_API_KEY`       | API key for the `venice` provider.                                              |
| `GITHUB_TOKEN`         | Token for the `github-copilot` provider.                                        |
| `AZURE_API_KEY`        | API key for the `azure` provider.                                               |
| `HF_TOKEN`             | Token for the `huggingface` provider.                                           |
| `NVIDIA_API_KEY`       | API key for the `nvidia` provider.                                              |
| `CLAURST_BRIDGE_URL`   | Relay address for the remote-control bridge. Overrides `remoteControl.url`.     |
| `CLAURST_BRIDGE_TOKEN` | Bearer token for the remote-control bridge. Overrides `remoteControl.token`.    |
| `RUST_LOG`             | Tracing filter (e.g. `debug`, `claurst_core=trace`).                            |

---

## Custom Slash Commands

User-defined slash commands can be added to the `commands` map:

```json
"commands": {
  "review": {
    "template": "Please review the following code for bugs and style: $ARGUMENTS",
    "description": "Review code",
    "agent": "plan",
    "model": null
  }
}
```

`CommandTemplate` fields:

| Field         | Description                                                                                    |
|---------------|------------------------------------------------------------------------------------------------|
| `template`    | Template string. `$ARGUMENTS` is replaced with whatever the user types after the command name. |
| `description` | Short description shown in `/help`.                                                            |
| `agent`       | Optional named agent to use (e.g. `"plan"`, `"build"`, `"explore"`).                           |
| `model`       | Optional model override for this command.                                                      |

Use the command with `/review path/to/file.rs`.

---

## Named Agents

Agents are named configurations that combine a system prompt prefix, model,
permission level, and turn limit. Three are built in:

| Agent     | Access      | Description                                                   |
|-----------|-------------|---------------------------------------------------------------|
| `build`   | full        | Read, write, and execute. For feature implementation.         |
| `plan`    | read-only   | Read files; no writes or commands. For analysis and planning. |
| `explore` | search-only | Search and read. For rapid codebase exploration.              |

You can define custom agents in `settings.json`:

```json
"agents": {
  "review": {
    "description": "Code review agent",
    "model": "anthropic/claude-haiku-4-5",
    "temperature": 0.3,
    "prompt": "You are a senior engineer doing code review. Be thorough and direct.",
    "access": "read-only",
    "visible": true,
    "max_turns": 30,
    "color": "magenta"
  }
}
```

`AgentDefinition` fields:

| Field         | Type            | Description                                                            |
|---------------|-----------------|------------------------------------------------------------------------|
| `description` | string \| null  | Description shown in `@agent` autocomplete.                            |
| `model`       | string \| null  | Model override for this agent.                                         |
| `temperature` | float \| null   | Sampling temperature override.                                         |
| `prompt`      | string \| null  | System prompt prefix (prepended before the main system prompt).        |
| `access`      | string          | Permission level: `"full"`, `"read-only"`, or `"search-only"`.         |
| `visible`     | boolean         | Whether to show in autocomplete. Default: true.                        |
| `max_turns`   | integer \| null | Maximum agentic turns.                                                 |
| `color`       | string \| null  | ANSI display color: `"cyan"`, `"magenta"`, `"green"`, `"yellow"`, etc. |

Invoke an agent with `@agentname` in the TUI or `--agent agentname` on the CLI.

---

## Managed Agents Configuration

The `managed_agents` key stores the managed-agents architecture configuration set via `/managed-agents configure`. It is written automatically by the command and rarely needs to be edited manually.

```json
"managed_agents": {
  "enabled": true,
  "manager_model": "anthropic/claude-opus-4-6",
  "executor_model": "anthropic/claude-sonnet-4-6",
  "executor_max_turns": 20,
  "max_concurrent": 3,
  "executor_isolation": true,
  "budget_split": {
    "type": "Percentage",
    "manager_pct": 20
  },
  "total_budget_usd": 5.00
}
```

`budget_split` types:

| Type         | JSON                                                                 | Description                        |
|--------------|----------------------------------------------------------------------|------------------------------------|
| `SharedPool` | `{ "type": "SharedPool" }`                                           | All agents draw from a single pool |
| `Percentage` | `{ "type": "Percentage", "manager_pct": 20 }`                        | Manager gets N% of total budget    |
| `FixedCaps`  | `{ "type": "FixedCaps", "manager_usd": 0.50, "executor_usd": 2.00 }` | Hard USD caps per role             |

Configure via `/managed-agents configure` or `/managed-agents preset <name>`. Set `enabled: false` to disable without removing the configuration.

---

## File Formatters

Formatters run automatically after Claurst writes a file whose extension
matches. They are defined in the `formatter` map:

```json
"formatter": {
  "prettier": {
    "command": ["prettier", "--write"],
    "extensions": [".ts", ".tsx", ".js", ".json"],
    "disabled": false
  },
  "rustfmt": {
    "command": ["rustfmt"],
    "extensions": [".rs"],
    "disabled": false
  }
}
```

| Field        | Description                                                       |
|--------------|-------------------------------------------------------------------|
| `command`    | Command array. The filename is appended as the final argument.    |
| `extensions` | File extensions this formatter handles (include the leading dot). |
| `disabled`   | Set to true to temporarily disable without removing the entry.    |

---

## Annotated Example `settings.json`

```json
{
  // Settings schema version
  "version": 1,

  // Active provider (can be overridden per-session with --provider)
  "provider": "anthropic",

  "config": {
    // Omit api_key here; use ANTHROPIC_API_KEY env var instead
    "api_key": null,

    // Model — leave null to use the provider's default
    "model": null,

    // Cap responses at 8 192 tokens
    "max_tokens": 8192,

    // In the TUI, ask before writing files or running commands
    "permission_mode": "default",

    // Dark theme for the TUI
    "theme": "dark",

    // Compact when context window is 85% full
    "auto_compact": true,
    "compact_threshold": 0.85,

    // Show debug logs
    "verbose": false,

    // Plain text output in --print mode
    "output_format": "text",

    // Add a custom instruction to every session
    "append_system_prompt": "Always explain your reasoning before making changes.",

    // Block the Bash tool globally
    "disallowed_tools": ["Bash"],

    // Inject a variable into every tool execution
    "env": {
      "MY_PROJECT_TOKEN": "{env:HOME}/.project_token"
    },

    // Run a script after every tool use
    "hooks": {
      "PostToolUse": [
        {
          "command": "/home/user/scripts/audit-log.sh",
          "blocking": false
        }
      ]
    },

    // Connect an MCP server at startup
    "mcp_servers": [
      {
        "name": "filesystem",
        "command": "mcp-server-filesystem",
        "args": ["/home/user/projects"],
        "env": {},
        "type": "stdio"
      }
    ]
  },

  // Per-provider credentials and options
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
    }
  },

  // Correct metadata for self-hosted / unknown models (keyed by provider/model).
  // Overrides win over the models.dev catalog.
  "modelOverrides": {
    "custom-openai/my-local-llm": {
      "contextWindow": 32768,
      "maxOutputTokens": 4096,
      "name": "My Local LLM"
    }
  },

  // Custom slash commands
  "commands": {
    "test": {
      "template": "Run the tests for $ARGUMENTS and report any failures.",
      "description": "Run and report tests"
    }
  },

  // Auto-run prettier on JS/TS file writes
  "formatter": {
    "prettier": {
      "command": ["prettier", "--write"],
      "extensions": [".ts", ".tsx", ".js", ".jsx"],
      "disabled": false
    }
  }
}
```
