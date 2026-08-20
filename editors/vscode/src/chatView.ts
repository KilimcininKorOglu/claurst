import * as vscode from 'vscode';
import { AgentPool } from './agentPool';
import { ChatPanel } from './chatPanel';
import { chooseWorkingFolder } from './workspace';

/** Names the sidebar view to VS Code. Must match `contributes.views`. */
export const CHAT_VIEW_ID = 'mikmik.chatView';

/** Puts a conversation in the Activity Bar.
 *
 * The same conversation code as the editor panel, drawn on a different kind of
 * webview. Which one a user reaches for is a habit rather than a feature, so
 * neither is the real one.
 *
 * The view is resolved once per window and keeps its conversation for as long
 * as the container exists. A second conversation is an editor panel: the
 * sidebar holds one view. */
export class ChatViewProvider implements vscode.WebviewViewProvider {
  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly pool: AgentPool,
    private readonly outputChannel: vscode.OutputChannel,
  ) {}

  async resolveWebviewView(
    view: vscode.WebviewView,
    context: vscode.WebviewViewResolveContext,
  ): Promise<void> {
    // The view is resolved again after a reload, and its own state is what
    // survived. Reopening the conversation it was showing beats starting an
    // empty one beside a transcript the user can no longer see.
    const saved = context.state as { sessionId?: unknown; cwd?: unknown; title?: unknown };
    if (typeof saved?.sessionId === 'string' && typeof saved?.cwd === 'string') {
      ChatPanel.inView(view, this.extensionUri, this.pool, this.outputChannel, {
        kind: 'load',
        session: {
          sessionId: saved.sessionId,
          cwd: saved.cwd,
          title: typeof saved.title === 'string' ? saved.title : undefined,
        },
      });
      return;
    }

    const cwd = await chooseWorkingFolder();
    if (!cwd) {
      // Opening the view is not the same as choosing a folder to work in. The
      // view stays, saying so, rather than guessing at a root.
      view.webview.html = emptyView('Run "MikMik: Open Chat" to start a conversation.');
      return;
    }
    ChatPanel.inView(view, this.extensionUri, this.pool, this.outputChannel, { kind: 'new', cwd });
  }
}

/** A view with nothing in it yet. Built here rather than left blank so the
 * sidebar says why. */
function emptyView(message: string): string {
  const escaped = message.replace(/[&<>]/g, (c) => `&#${c.charCodeAt(0)};`);
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline';" />
</head>
<body style="font-family: var(--vscode-font-family); padding: 12px;">${escaped}</body>
</html>`;
}
