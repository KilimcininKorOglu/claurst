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
| `Claurst: Open Chat`            | Reveals a panel, or opens one                  |
| `Claurst: New Session`          | Opens another panel with its own conversation  |
| `Claurst: Resume Session`       | Lists earlier sessions and reopens one         |
| `Claurst: Stop Current Turn`    | Cancels the turn in the focused panel          |

## One process, many conversations

Every panel is a session inside a single `claurst acp` process, so a second
conversation costs a session rather than a process: the MCP servers are
connected once and the model catalog is read once. The process starts with the
first panel and stops with the last.

Each panel is independent. A permission request, a plan and a diff reach the
panel whose session raised them, and `Claurst: Stop Current Turn` cancels the
one you are looking at.

## The header pills

The pills are not hardcoded: the agent reports what it offers in its
`session/new` response, and each pill sends the choice back through
`session/set_config_option` or `session/set_mode`. Today that is the model, the
account, the reasoning effort, and how permission requests are answered.

Changing one restates the others, because the model list belongs to the account
and the effort ladder belongs to the model.

These choices apply to the running session only. Nothing is written to
`settings.json`, so a session started from a terminal is unaffected.

## Slash commands and file mentions

Typing `/` lists the commands the agent reported, with what each one takes;
arrow keys and Tab pick one. The command runs in the agent, not in the model.

Typing `@` opens a file picker. A small file travels with the prompt so the
agent does not spend a turn reading it; a larger one is named, and the agent
reads the part it needs.

## What a turn shows

A tool that rewrote files shows each one as a line-by-line diff. A plan the
agent stored appears above the transcript as a checklist and is redrawn as it
moves.

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

No image or audio input: the agent drops both, and its `initialize` says so.
Terminals stay with the agent rather than being hosted by the editor.
