# Claurst for VS Code

Chat with Claurst inside VS Code. The extension spawns `claurst acp` as a child
process and speaks the [Agent Client Protocol](https://agentclientprotocol.com)
to it over stdio, the same protocol Zed and other editors use, so nothing here
depends on private interfaces.

## Requirements

A `claurst` binary on `PATH`, or a path set in `claurst.executablePath`.

## Where the chat lives

Two places, and neither is the real one: an entry in the Activity Bar, and an
editor tab. The sidebar holds one conversation; a second is a tab. Both run the
same code on the two kinds of webview VS Code offers.

A conversation open when the window closes is reopened on the next launch, with
its transcript replayed.

## Commands

| Command | Key | What it does |
|------------------------------|-----------------|----------------------------------------------|
| `Claurst: Open Chat`         | `Ctrl/Cmd+Shift+A` | Reveals a conversation, or opens one       |
| `Claurst: New Session`       |                 | Opens another panel with its own conversation |
| `Claurst: Resume Session`    |                 | Lists earlier sessions and reopens one        |
| `Claurst: Fork Session`      |                 | Continues this conversation in a second panel |
| `Claurst: Send Selection`    | `Ctrl/Cmd+Shift+L` | Puts the editor's selection in the question |
| `Claurst: Stop Current Turn` | `Escape`        | Cancels the turn in the focused panel         |

Resuming asks whether to draw the earlier conversation or just carry on:
replaying a long one costs a message per block, so it is a choice rather than
an assumption.

In a workspace with more than one folder, opening a conversation asks which one
to work in. The folder decides what the agent can see, what a mention resolves
against, and which stored sessions the resume list returns.

Forking leaves the original untouched. The new panel carries the conversation
so far and the choices the source made, which is how you try a second approach
without losing the first.

## One process, many conversations

Every panel is a session inside a single `claurst acp` process, so a second
conversation costs a session rather than a process: the MCP servers are
connected once and the model catalog is read once. The process starts with the
first panel and stops with the last.

Each conversation is independent. A permission request, a plan and a diff reach
the panel whose session raised them, and `Claurst: Stop Current Turn` cancels the
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

## Asking

Typing `/` lists the commands the agent reported; arrow keys and Tab pick one.
Once the name is settled the list becomes the argument hint for that command,
which is when it is worth reading. The command runs in the agent, not in the
model.

Typing `@` opens a file picker, and the path lands where the `@` was typed. A
small file travels with the prompt so the agent does not spend a turn reading
it; a larger one is named, and the agent reads the part it needs. The picker
respects the project's own `files.exclude` and `search.exclude`.

Pasting an image attaches it to the next question, so a screenshot of a failing
test goes straight in rather than being described. This is offered only when
the agent's `initialize` says it accepts images.

`Claurst: Send Selection` mentions the file, says which lines, and fences the
selected text.

## What a turn shows

Answers are rendered as markdown with highlighted code. Each code block has a
copy button and an apply button, which writes into the editor's selection
through a workspace edit so one Undo takes it back out.

A tool call names the files it is working on, and clicking one opens it at the
line. A tool that rewrote files shows each one as a line-by-line diff. Reasoning
is folded away so it does not bury the answer it led to. A plan the agent stored
appears above the transcript as a checklist and is redrawn as it moves.

Scrolling up during a turn stays where you put it; a button appears to say there
is more below.

## Permissions

A tool that needs approval asks in the transcript, next to the call it is about,
showing what it would do: a write as a diff against the file on disk, an edit as
the text it replaces and the text replacing it, a command as the command. Four
answers are offered, including allowing or refusing this tool from now on. The
block stays afterwards saying which one was chosen.

Closing the panel without answering is not consent: the call is refused.

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

## When the agent stops

A crashed process takes every session with it. The panel says so, with the exit
code and the last lines the process printed, and offers to start another and
replay the conversation into the same panel. The status bar says whether the
agent is running and whether it is answering.

`claurst.executablePath`, `claurst.hostTerminals` and
`claurst.requestTimeoutSeconds` are read when the process starts, so changing
one offers to restart it.

## Developing

```bash
npm install
npm run compile     # or: npm run watch
npm run check       # type-check the host, the webview and the tests
npm test
npm run package     # build a .vsix
```

Then press F5 to open an Extension Development Host running this extension.

The extension is two bundles built by `esbuild.mjs`: the host, which runs in
Node with `vscode` injected, and the webview, which runs in a browser sandbox
where neither exists. They share no module. Anything that only reads the
protocol lives in `src/protocol.ts`, which imports nothing, and is what the
tests exercise.

## Scope

No audio input: the agent has no way to carry it to the model, and its
`initialize` says so.

Publishing to the marketplace is manual. CI packages the extension so a
manifest that cannot be packaged fails there.
