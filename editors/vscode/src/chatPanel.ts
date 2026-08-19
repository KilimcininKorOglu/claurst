import * as os from 'os';
import * as path from 'path';
import * as vscode from 'vscode';
import {
  AcpClient,
  AvailableCommand,
  ChunkKind,
  ConfigOption,
  PermissionAnswer,
  PermissionOption,
  PlanEntry,
  PromptBlock,
  SessionModes,
  StoredSession,
  ToolCallUpdate,
} from './acpClient';
import { AgentPool } from './agentPool';

/** The key the mode pill uses. Config option ids come from the agent, so this
 * one only has to avoid colliding with them. */
const MODE_PILL = 'mode';

/** The largest file whose contents travel with the prompt. Anything bigger is
 * named instead, so the agent reads the part it needs. */
const EMBED_LIMIT_BYTES = 64 * 1024;

/** How a panel's session comes to exist. */
export type PanelOpening =
  | { kind: 'new' }
  /** Reopen a stored session and draw the whole conversation back. */
  | { kind: 'load'; session: StoredSession }
  /** Reopen it and keep going, without re-reading it. */
  | { kind: 'resume'; session: StoredSession }
  /** Continue another session's conversation under a new id. */
  | { kind: 'fork'; sessionId: string; cwd: string; title?: string };

/** The directory a panel opened this way works in. */
function cwdOf(opening: PanelOpening | undefined): string | undefined {
  switch (opening?.kind) {
    case 'load':
    case 'resume':
      return opening.session.cwd;
    case 'fork':
      return opening.cwd;
    default:
      return undefined;
  }
}

/** What to call a panel before the agent names its session. */
function titleOf(opening: PanelOpening | undefined): string | undefined {
  switch (opening?.kind) {
    case 'load':
    case 'resume':
      return opening.session.title;
    case 'fork':
      return opening.title ? `${opening.title} (fork)` : undefined;
    default:
      return undefined;
  }
}

/** Owns one webview panel and the session behind it.
 *
 * Panels are independent conversations sharing a single agent process, so
 * opening a second one costs a session rather than a process. */
export class ChatPanel {
  private static readonly panels = new Set<ChatPanel>();
  /** The panel the user is looking at, which the palette commands act on. */
  static active: ChatPanel | undefined;

  private readonly panel: vscode.WebviewPanel;
  private client: AcpClient | undefined;
  private sessionId: string | undefined;
  private disposables: vscode.Disposable[] = [];

  private options: ConfigOption[] = [];
  private modes: SessionModes | undefined;
  private cwd: string;
  /** Questions the webview has not answered yet, by the id it will answer with.
   * A panel that closes settles them rather than leaving the agent waiting. */
  private pendingPermissions = new Map<number, (answer: PermissionAnswer) => void>();
  private nextPermissionId = 1;
  /** Whether this panel is holding the shared agent. Releasing one it never
   * took would shut the process down under another panel. */
  private holdsAgent = false;

  static create(
    extensionUri: vscode.Uri,
    pool: AgentPool,
    outputChannel: vscode.OutputChannel,
    opening: PanelOpening = { kind: 'new' },
  ): ChatPanel {
    const panel = vscode.window.createWebviewPanel(
      'claurstChat',
      titleOf(opening) ?? 'Claurst',
      vscode.ViewColumn.Beside,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [
          vscode.Uri.joinPath(extensionUri, 'media'),
          vscode.Uri.joinPath(extensionUri, 'out'),
        ],
      },
    );
    return new ChatPanel(panel, extensionUri, pool, outputChannel, opening);
  }

  /** Reveal a panel if there is one, otherwise start one. */
  static show(
    extensionUri: vscode.Uri,
    pool: AgentPool,
    outputChannel: vscode.OutputChannel,
  ): ChatPanel {
    const existing = ChatPanel.active ?? ChatPanel.panels.values().next().value;
    if (existing) {
      existing.panel.reveal();
      return existing;
    }
    return ChatPanel.create(extensionUri, pool, outputChannel);
  }

  static disposeAll(): void {
    for (const panel of [...ChatPanel.panels]) {
      panel.dispose();
    }
  }

  private constructor(
    panel: vscode.WebviewPanel,
    extensionUri: vscode.Uri,
    private readonly pool: AgentPool,
    private readonly outputChannel: vscode.OutputChannel,
    private readonly opening: PanelOpening = { kind: 'new' },
  ) {
    this.panel = panel;
    this.cwd =
      cwdOf(opening) ?? vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? os.homedir();
    ChatPanel.panels.add(this);
    ChatPanel.active = this;

    this.panel.webview.html = this.renderHtml(extensionUri);
    this.panel.onDidDispose(() => this.dispose(), null, this.disposables);
    this.panel.onDidChangeViewState(
      () => {
        if (this.panel.active) {
          ChatPanel.active = this;
        }
      },
      null,
      this.disposables,
    );
    this.panel.webview.onDidReceiveMessage(
      (msg) => this.handleWebviewMessage(msg),
      null,
      this.disposables,
    );
    this.startSession().catch((e) => this.reportError(e));
  }

  private renderHtml(extensionUri: vscode.Uri): string {
    const webview = this.panel.webview;
    const scriptUri = webview.asWebviewUri(vscode.Uri.joinPath(extensionUri, 'out', 'webview.js'));
    const styleUri = webview.asWebviewUri(vscode.Uri.joinPath(extensionUri, 'media', 'main.css'));
    // A CSP nonce only keeps injected script out if it cannot be guessed, and
    // Math.random is seeded predictably enough that it can be.
    const nonce = crypto.randomUUID().replace(/-/g, '');
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <!-- img-src is data: only. Allowing a remote origin would let an image the
       agent chose reach the network from inside the panel. -->
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource}; img-src data:; script-src 'nonce-${nonce}';" />
  <link href="${styleUri}" rel="stylesheet" />
  <title>Claurst</title>
</head>
<body>
  <div id="header"></div>
  <div id="plan" class="hidden"></div>
  <div id="messages"></div>
  <button id="jump-btn" class="hidden" title="Jump to the end of the conversation">New messages ↓</button>
  <div id="completions" class="hidden"></div>
  <div id="input-row">
    <textarea id="input-box" rows="1" placeholder="Ask claurst... (/ for commands, @ for files)"></textarea>
    <button id="send-btn" title="Send (Enter)">Send</button>
    <button id="stop-btn" title="Cancel the current turn">Stop</button>
  </div>
  <script nonce="${nonce}" src="${scriptUri}"></script>
</body>
</html>`;
  }

  private async startSession(): Promise<void> {
    const client = await this.pool.acquire(this.cwd);
    this.holdsAgent = true;
    this.client = client;

    const events = {
      onTextChunk: (text: string, kind: ChunkKind) =>
        this.postToWebview({ type: 'textChunk', text, kind }),
      onImage: (mimeType: string, data: string, kind: ChunkKind) =>
        this.postToWebview({ type: 'image', mimeType, data, kind }),
      onToolCall: (update: ToolCallUpdate) =>
        this.postToWebview({ type: 'toolCall', ...toolCallPayload(update) }),
      onToolCallUpdate: (update: ToolCallUpdate) =>
        this.postToWebview({ type: 'toolCallUpdate', ...toolCallPayload(update) }),
      onRequestPermission: (toolCall: ToolCallUpdate, options: PermissionOption[]) =>
        this.promptForPermission(toolCall, options),
      onConfigOptions: (options: ConfigOption[]) => {
        this.options = options;
        this.pushHeader();
      },
      onModeChanged: (modeId: string) => {
        if (this.modes) {
          this.modes = { ...this.modes, currentModeId: modeId };
          this.pushHeader();
        }
      },
      onCommands: (commands: AvailableCommand[]) =>
        this.postToWebview({ type: 'commands', commands }),
      onPlan: (entries: PlanEntry[]) => this.postToWebview({ type: 'plan', entries }),
      onSessionInfo: (title?: string) => {
        if (title) {
          this.panel.title = title;
        }
      },
      onTerminalOutput: (terminalId: string, chunk: string) =>
        this.postToWebview({ type: 'terminalOutput', terminalId, chunk }),
    };

    try {
      const opening = this.opening;
      let session;
      let opened: string;
      switch (opening.kind) {
        case 'load':
          session = await client.loadSession(opening.session.sessionId, this.cwd, events);
          opened = `Reopened ${opening.session.title ?? opening.session.sessionId}`;
          break;
        case 'resume':
          session = await client.resumeSession(opening.session.sessionId, this.cwd, events);
          opened = `Continuing ${opening.session.title ?? opening.session.sessionId}`;
          break;
        case 'fork':
          session = await client.forkSession(opening.sessionId, this.cwd, events);
          opened = `Forked ${opening.title ?? opening.sessionId}`;
          break;
        default:
          session = await client.newSession(this.cwd, events);
          opened = `Session started in ${this.cwd}`;
      }
      this.sessionId = session.sessionId;
      this.options = session.configOptions;
      this.modes = session.modes;
      this.pushHeader();
      this.postToWebview({ type: 'status', text: `${opened}${agentSuffix(client)}` });
    } catch (e) {
      this.reportError(e);
    }
  }

  /** What this panel is talking to, for a command that acts on it. */
  get session(): { sessionId: string; cwd: string; title: string } | undefined {
    return this.sessionId
      ? { sessionId: this.sessionId, cwd: this.cwd, title: this.panel.title }
      : undefined;
  }

  /** The pills, straight from what the agent said it offers. */
  private pushHeader(): void {
    const pills = this.options.map((option) => ({
      key: option.id,
      label: option.name.toLowerCase(),
      value: option.currentValue,
    }));
    if (this.modes) {
      const current = this.modes.availableModes.find((m) => m.id === this.modes?.currentModeId);
      pills.push({ key: MODE_PILL, label: 'mode', value: current?.name ?? this.modes.currentModeId });
    }
    this.postToWebview({ type: 'header', pills });
  }

  /** Ask in the panel rather than in a modal picker.
   *
   * A quick pick can only list the option names: it covers the conversation,
   * and it has nowhere to put the diff or the command the agent sent along. The
   * question belongs in the transcript next to the tool call it is about, and
   * it stays there afterwards as a record of what was approved. */
  private promptForPermission(
    toolCall: ToolCallUpdate,
    options: PermissionOption[],
  ): Promise<PermissionAnswer> {
    const requestId = this.nextPermissionId++;
    return new Promise<PermissionAnswer>((resolve) => {
      this.pendingPermissions.set(requestId, resolve);
      this.postToWebview({
        type: 'permission',
        requestId,
        title: toolCall.title ?? 'Claurst is requesting permission',
        description: toolCall.output,
        diffs: toolCall.diffs,
        locations: toolCall.locations,
        options: options.map((o) => ({ optionId: o.optionId, name: o.name, kind: o.kind })),
      });
    });
  }

  /** Settle one question the webview answered. */
  private answerPermission(requestId: unknown, optionId: unknown): void {
    if (typeof requestId !== 'number') {
      return;
    }
    const resolve = this.pendingPermissions.get(requestId);
    if (!resolve) {
      return;
    }
    this.pendingPermissions.delete(requestId);
    resolve(typeof optionId === 'string' ? { optionId } : { cancelled: true });
  }

  private handleWebviewMessage(msg: any): void {
    switch (msg.type) {
      case 'prompt':
        if (typeof msg.text === 'string') {
          this.runPrompt(msg.text);
        }
        break;
      case 'stop':
        this.cancelCurrentTurn();
        break;
      case 'pick':
        if (typeof msg.key === 'string') {
          this.pick(msg.key).catch((e) => this.reportError(e));
        }
        break;
      case 'pickFile':
        this.pickFile().catch((e) => this.reportError(e));
        break;
      case 'permissionAnswer':
        this.answerPermission(msg.requestId, msg.optionId);
        break;
      case 'applyCode':
        if (typeof msg.text === 'string') {
          applyCode(msg.text).catch((e) => this.reportError(e));
        }
        break;
      case 'openLocation':
        if (typeof msg.path === 'string') {
          openLocation(msg.path, typeof msg.line === 'number' ? msg.line : undefined).catch((e) =>
            this.reportError(e),
          );
        }
        break;
      default:
        break;
    }
  }

  /** Let the user choose a file to mention, and put it in the input box. */
  private async pickFile(): Promise<void> {
    const files = await vscode.workspace.findFiles('**/*', '**/{node_modules,.git,target}/**', 500);
    const items = files.map((uri) => ({
      label: vscode.workspace.asRelativePath(uri),
      uri,
    }));
    const picked = await vscode.window.showQuickPick(items, {
      placeHolder: 'Which file should Claurst look at?',
      matchOnDescription: true,
    });
    if (picked) {
      this.postToWebview({ type: 'mention', text: picked.label });
    }
  }

  /** Open a picker for one pill and send the choice to the agent. */
  private async pick(key: string): Promise<void> {
    const client = this.client;
    const sessionId = this.sessionId;
    if (!client || !sessionId) {
      return;
    }
    if (key === MODE_PILL) {
      const modes = this.modes;
      if (!modes) {
        return;
      }
      const picked = await vscode.window.showQuickPick(
        modes.availableModes.map((m) => ({ label: m.name, description: m.description, id: m.id })),
        { placeHolder: 'How should Claurst answer permission requests?', ignoreFocusOut: true },
      );
      if (!picked) {
        return;
      }
      await client.setMode(sessionId, picked.id);
      this.modes = { ...modes, currentModeId: picked.id };
      this.pushHeader();
      return;
    }

    const option = this.options.find((o) => o.id === key);
    if (!option) {
      return;
    }
    const picked = await vscode.window.showQuickPick(
      option.values.map((v) => ({
        label: v.name,
        description: v.value === option.currentValue ? 'current' : undefined,
        value: v.value,
      })),
      { placeHolder: `${option.name} (current: ${option.currentValue})`, ignoreFocusOut: true },
    );
    if (!picked) {
      return;
    }
    // Setting one option restates all of them: the model list belongs to the
    // account, and the effort ladder belongs to the model.
    this.options = await client.setConfigOption(sessionId, option.id, picked.value);
    this.pushHeader();
  }

  /** Runs a prompt to completion, signalling turnEnded either way so the
   * webview can re-enable input. */
  private async runPrompt(text: string): Promise<void> {
    try {
      if (this.client && this.sessionId) {
        await this.client.prompt(this.sessionId, await this.buildPrompt(text));
      }
    } catch (e) {
      this.reportError(e);
    } finally {
      this.postToWebview({ type: 'turnEnded' });
    }
  }

  /** Turn what was typed into prompt blocks, resolving any `@file` mention.
   *
   * A small file travels with the prompt so the agent does not have to spend a
   * turn reading it; a large one is named, and the agent reads the part it
   * needs. */
  private async buildPrompt(text: string): Promise<PromptBlock[]> {
    const blocks: PromptBlock[] = [{ type: 'text', text }];
    const roots = vscode.workspace.workspaceFolders ?? [];
    for (const mention of mentionsIn(text)) {
      const uri = await this.resolveMention(mention, roots);
      if (!uri) {
        continue;
      }
      let stat: vscode.FileStat;
      try {
        stat = await vscode.workspace.fs.stat(uri);
      } catch {
        // A mention that names nothing is just text; the model sees it either
        // way, and inventing a file would be worse.
        continue;
      }
      // An agent that did not claim embedded context would drop the contents,
      // so the file is named and it reads what it needs.
      if (stat.size > EMBED_LIMIT_BYTES || !this.client?.agent.embeddedContext) {
        blocks.push({ type: 'resource_link', uri: uri.toString(), name: mention });
        continue;
      }
      try {
        const bytes = await vscode.workspace.fs.readFile(uri);
        blocks.push({
          type: 'resource',
          resource: { uri: uri.toString(), text: Buffer.from(bytes).toString('utf8') },
        });
      } catch {
        blocks.push({ type: 'resource_link', uri: uri.toString(), name: mention });
      }
    }
    return blocks;
  }

  private async resolveMention(
    mention: string,
    roots: readonly vscode.WorkspaceFolder[],
  ): Promise<vscode.Uri | undefined> {
    if (path.isAbsolute(mention)) {
      return vscode.Uri.file(mention);
    }
    for (const root of roots) {
      const candidate = vscode.Uri.joinPath(root.uri, mention);
      try {
        await vscode.workspace.fs.stat(candidate);
        return candidate;
      } catch {
        // Try the next root; a mention need not belong to the first one.
      }
    }
    return undefined;
  }

  cancelCurrentTurn(): void {
    if (this.client && this.sessionId) {
      this.client.cancel(this.sessionId);
    }
  }

  private postToWebview(msg: unknown): void {
    this.panel.webview.postMessage(msg);
  }

  private reportError(e: unknown): void {
    const message = e instanceof Error ? e.message : String(e);
    this.outputChannel.appendLine(`[claurst-vscode] ${message}`);
    this.postToWebview({ type: 'status', text: `Error: ${message}` });
  }

  dispose(): void {
    if (!ChatPanel.panels.delete(this)) {
      // Already disposed: the panel's own onDidDispose and an explicit call
      // both land here.
      return;
    }
    if (ChatPanel.active === this) {
      ChatPanel.active = ChatPanel.panels.values().next().value;
    }
    // Closing the panel is not consent. Anything still waiting on an answer is
    // told nobody chose, which denies the call rather than stalling the turn.
    for (const resolve of this.pendingPermissions.values()) {
      resolve({ cancelled: true });
    }
    this.pendingPermissions.clear();
    if (this.client && this.sessionId) {
      const client = this.client;
      const sessionId = this.sessionId;
      // The session is written out by the agent as it closes, so a panel
      // closed by accident can be reopened from the session list.
      client.closeSession(sessionId).catch(() => undefined);
    }
    if (this.holdsAgent) {
      this.holdsAgent = false;
      this.pool.release();
    }
    this.panel.dispose();
    for (const d of this.disposables) {
      d.dispose();
    }
    this.disposables = [];
  }
}

/** Put a code block from the transcript into the editor.
 *
 * It replaces the selection, or is inserted at the cursor when there is none.
 * A workspace edit rather than a direct write, so one Undo takes it back out
 * again: the user is trying it, not committing to it. */
async function applyCode(text: string): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor) {
    // Silently doing nothing would look like the button is broken.
    vscode.window.showWarningMessage('Open a file to apply this code to.');
    return;
  }
  const edit = new vscode.WorkspaceEdit();
  edit.replace(editor.document.uri, editor.selection, text);
  if (!(await vscode.workspace.applyEdit(edit))) {
    throw new Error('the workspace refused the edit');
  }
}

/** Name the agent behind a session, when it introduced itself.
 *
 * Which build is answering matters when two versions are installed and the
 * panel behaves differently from the terminal. */
function agentSuffix(client: AcpClient): string {
  const { name, version } = client.agent;
  if (!name) {
    return '';
  }
  return version ? ` (${name} ${version})` : ` (${name})`;
}

/** Open the file a tool call named, at the line it named.
 *
 * The agent reports absolute paths, so the document is addressed directly
 * rather than searched for; a path outside the workspace still opens, which is
 * what the agent said it was working on. */
async function openLocation(target: string, line?: number): Promise<void> {
  const doc = await vscode.workspace.openTextDocument(vscode.Uri.file(target));
  const editor = await vscode.window.showTextDocument(doc, { preview: true });
  if (line === undefined) {
    return;
  }
  // The protocol counts from 1 and the editor counts from 0. A line past the
  // end of the file is clamped rather than refused: the file may have changed
  // since the agent looked at it.
  const zeroBased = Math.min(Math.max(line - 1, 0), Math.max(doc.lineCount - 1, 0));
  const at = new vscode.Range(zeroBased, 0, zeroBased, 0);
  editor.selection = new vscode.Selection(at.start, at.start);
  editor.revealRange(at, vscode.TextEditorRevealType.InCenterIfOutsideViewport);
}

/** Every `@path` mention in what the user typed. */
function mentionsIn(text: string): string[] {
  const found = new Set<string>();
  const pattern = /(^|\s)@([^\s]+)/g;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(text)) !== null) {
    found.add(match[2]);
  }
  return [...found];
}

function toolCallPayload(update: ToolCallUpdate) {
  return {
    toolCallId: update.toolCallId,
    title: update.title,
    status: update.status,
    kind: update.kind,
    locations: update.locations,
    output: update.output,
    diffs: update.diffs,
    terminalId: update.terminalId,
  };
}
