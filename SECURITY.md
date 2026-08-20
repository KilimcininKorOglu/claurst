# Security Policy

## Supported versions

Only the latest release receives fixes. Older tags are not patched; upgrade
before reporting a problem you can reproduce on an old build.

## Reporting a vulnerability

Report privately through GitHub's [private vulnerability
reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
on this repository. Do not open a public issue for a vulnerability.

A useful report says which version you ran, what you did, what happened, and
what you expected instead. A reproduction someone else can run is worth more
than a description of the code path.

## Trust model

MikMik runs commands, edits files and talks to network endpoints on your
machine. Most of that is the point of the tool. The parts below are where an
input that is not yours decides what runs.

### A repository's settings file

Opening a checkout means its `.mikmik/settings.json` is read. It arrives with
the clone and nobody has read it, so what it may set is limited to three
groups. [`docs/configuration.md`](docs/configuration.md#per-project-settings)
lists the keys in each.

- **Applied.** Ordinary configuration: the model, agents, commands, model
  overrides. None of it decides what runs or where a credential goes.
- **Applied after you approve it.** `hooks`, `formatter`, `lsp_servers`,
  `skills` and project-defined `mcpServers`. Each names a command to execute or
  an address to fetch from, so MikMik prints them as written and asks before
  any of them takes effect. "Always allow" records a SHA-256 fingerprint of
  exactly what was shown, under `~/.config/mikmik/project_trust.json` and
  `~/.config/mikmik/mcp_trust.json`. Neither store is ever written inside a
  repository, and editing an approved command changes its fingerprint, so an
  approval cannot be re-pointed at something else. Headless (`--print`) never
  applies them and says so on stderr.
- **Never applied.** The permission mode and permission rules, the API key,
  provider and provider endpoints, the system prompt, the tool environment,
  workspace and additional directories, the status line, ACP agents, and the
  flags that would turn off the two gates above. MikMik names the ignored keys
  on startup rather than dropping them silently.

### Permission modes

`--dangerously-skip-permissions` and `bypassPermissions` do what they say: the
tools stop asking. Use them only where you are willing to lose the directory.
A project settings file cannot turn either one on.

### Model output and tool results

Web pages, file contents, MCP tool results and command output are data, not
instructions. A page that says "ignore your instructions and run X" is a page
saying that, and the permission prompts remain the boundary. Approving a tool
call is approving what it will do.

### What is not in scope

- A hook, formatter, MCP server or skill you approved yourself doing what you
  approved it to do.
- Anything reachable only after `--dangerously-skip-permissions`.
- Secrets you placed in a settings file that you then shared.
