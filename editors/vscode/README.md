# Claurst for VS Code

Chat with Claurst inside VS Code. The extension spawns `claurst acp` as a child
process and speaks the [Agent Client Protocol](https://agentclientprotocol.com)
to it over stdio, the same protocol Zed and other editors use, so nothing here
depends on private interfaces.

## Requirements

A `claurst` binary on `PATH`, or a path set in `claurst.executablePath`.

## Commands

| Command | What it does |
|---------------------------------|-----------------------------------------------|
| `Claurst: Open Chat`            | Opens the panel and starts a session           |
| `Claurst: New Session`          | Discards the current session and starts a new one |
| `Claurst: Stop Current Turn`    | Cancels the turn in flight                     |

## The header pills

The pills are not hardcoded: the agent reports what it offers in its
`session/new` response, and each pill sends the choice back through
`session/set_config_option` or `session/set_mode`. Today that is the model, the
account, the reasoning effort, and how permission requests are answered.

Changing one restates the others, because the model list belongs to the account
and the effort ladder belongs to the model.

These choices apply to the running session only. Nothing is written to
`settings.json`, so a session started from a terminal is unaffected.

## Permissions

A tool that needs approval opens a quick pick. Dismissing it without choosing
cancels the turn: walking away from the question is not consent, and the tool
does not run.

## Developing

```bash
npm install
npm run compile     # or: npm run watch
```

Then press F5 to open an Extension Development Host running this extension.

## Scope

One session per window, no inline diffs, no `@file` mentions, no session
resume. `session/load` is not implemented by the agent either.
