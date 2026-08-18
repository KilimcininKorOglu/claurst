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
| `Claurst: Fork Session`         | Continues this conversation in a second panel  |
| `Claurst: Stop Current Turn`    | Cancels the turn in the focused panel          |

Resuming asks whether to draw the earlier conversation or just carry on:
replaying a long one costs a message per block, so it is a choice rather than
an assumption.

Forking leaves the original untouched. The new panel carries the conversation
so far and the choices the source made, which is how you try a second approach
without losing the first.

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

A tool that needs approval opens a quick pick showing what it is about to do:
a write appears as a diff against the file on disk, an edit as the text it
replaces and the text replacing it, a command as the command. Dismissing the
pick without choosing cancels the turn: walking away from the question is not
consent, and the tool does not run.

## Files the agent reads and writes

The extension hosts the files. A read comes from the buffer you are looking at,
so edits you have not saved are what the agent sees, and a write goes through a
workspace edit, so it is undoable and shows up in the editor rather than
underneath it. A file no editor has open is read and written through the
workspace filesystem, which also covers a remote workspace.

## Running commands here instead

`claurst.hostTerminals` moves the agent's shell commands into this extension,
so their output appears live in the panel under the call that started them.

It is off by default, and the reason is not cosmetic: the agent runs a command
in a real PTY, and this runs it on a pipe. Anything that checks whether it is
attached to a terminal (npm, cargo, git, pytest) prints differently, and some
tools disable colour or progress entirely.

## Developing

```bash
npm install
npm run compile     # or: npm run watch
```

Then press F5 to open an Extension Development Host running this extension.

## Scope

No audio input: the agent has no way to carry it to the model, and its
`initialize` says so.
