import * as os from 'os';
import * as vscode from 'vscode';
import {
  AcpClient,
  ConfigOption,
  PermissionAnswer,
  PermissionOption,
  SessionModes,
  ToolCallUpdate,
} from './acpClient';

/** The key the mode pill uses. Config option ids come from the agent, so this
 * one only has to avoid colliding with them. */
const MODE_PILL = 'mode';

/** Owns one webview panel and its backing AcpClient/session. */
export class ChatPanel {
  public static current: ChatPanel | undefined;

  private readonly panel: vscode.WebviewPanel;
  private client: AcpClient | undefined;
  private readonly outputChannel: vscode.OutputChannel;
  private disposables: vscode.Disposable[] = [];

  private options: ConfigOption[] = [];
  private modes: SessionModes | undefined;

  static createOrShow(
    extensionUri: vscode.Uri,
    version: string,
    outputChannel: vscode.OutputChannel,
  ): ChatPanel {
    if (ChatPanel.current) {
      ChatPanel.current.panel.reveal();
      return ChatPanel.current;
    }
    const panel = vscode.window.createWebviewPanel(
      'claurstChat',
      'Claurst',
      vscode.ViewColumn.Beside,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [vscode.Uri.joinPath(extensionUri, 'media')],
      },
    );
    ChatPanel.current = new ChatPanel(panel, extensionUri, version, outputChannel);
    return ChatPanel.current;
  }

  private constructor(
    panel: vscode.WebviewPanel,
    extensionUri: vscode.Uri,
    private readonly version: string,
    outputChannel: vscode.OutputChannel,
  ) {
    this.panel = panel;
    this.outputChannel = outputChannel;
    this.panel.webview.html = this.renderHtml(extensionUri);
    this.panel.onDidDispose(() => this.dispose(), null, this.disposables);
    this.panel.webview.onDidReceiveMessage(
      (msg) => this.handleWebviewMessage(msg),
      null,
      this.disposables,
    );
    this.startSession().catch((e) => this.reportError(e));
  }

  private renderHtml(extensionUri: vscode.Uri): string {
    const webview = this.panel.webview;
    const scriptUri = webview.asWebviewUri(vscode.Uri.joinPath(extensionUri, 'media', 'main.js'));
    const styleUri = webview.asWebviewUri(vscode.Uri.joinPath(extensionUri, 'media', 'main.css'));
    const nonce = String(Math.random()).slice(2);
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource}; script-src 'nonce-${nonce}';" />
  <link href="${styleUri}" rel="stylesheet" />
  <title>Claurst</title>
</head>
<body>
  <div id="header"></div>
  <div id="messages"></div>
  <div id="input-row">
    <textarea id="input-box" rows="1" placeholder="Ask claurst..."></textarea>
    <button id="send-btn" title="Send (Enter)">Send</button>
    <button id="stop-btn" title="Cancel the current turn">Stop</button>
  </div>
  <script nonce="${nonce}" src="${scriptUri}"></script>
</body>
</html>`;
  }

  private async startSession(): Promise<void> {
    // Prefer the first workspace folder, but don't block chat on one being
    // open — fall back to the user's home directory so the panel is always
    // usable, matching how a plain terminal session would behave.
    const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? os.homedir();
    const executablePath = vscode.workspace
      .getConfiguration('claurst')
      .get<string>('executablePath', 'claurst');

    this.client = new AcpClient(executablePath, cwd, this.version, {
      onTextChunk: (text, isThought) => this.postToWebview({ type: 'textChunk', text, isThought }),
      onToolCall: (update) => this.postToWebview({ type: 'toolCall', ...toolCallPayload(update) }),
      onToolCallUpdate: (update) =>
        this.postToWebview({ type: 'toolCallUpdate', ...toolCallPayload(update) }),
      onRequestPermission: (toolCall, options) => this.promptForPermission(toolCall, options),
      onConfigOptions: (options) => {
        this.options = options;
        this.pushHeader();
      },
      onModeChanged: (modeId) => {
        if (this.modes) {
          this.modes = { ...this.modes, currentModeId: modeId };
          this.pushHeader();
        }
      },
      onStderr: (line) => this.outputChannel.appendLine(line),
      onExit: (code) => {
        this.postToWebview({
          type: 'status',
          text: `claurst process exited (code ${code ?? 'unknown'}).`,
        });
      },
    });

    try {
      await this.client.initialize();
      const session = await this.client.newSession(cwd);
      this.options = session.configOptions;
      this.modes = session.modes;
      this.pushHeader();
      this.postToWebview({ type: 'status', text: `Session started in ${cwd}` });
    } catch (e) {
      this.reportError(e);
    }
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

  private async promptForPermission(
    toolCall: ToolCallUpdate,
    options: PermissionOption[],
  ): Promise<PermissionAnswer> {
    const picked = await vscode.window.showQuickPick(
      options.map((o) => ({ label: o.name, description: o.kind, optionId: o.optionId })),
      {
        placeHolder: toolCall.title ?? 'Claurst is requesting permission',
        ignoreFocusOut: true,
      },
    );
    // Dismissing the picker is not consent. Cancelling ends the turn, which is
    // what the user asked for by walking away from the question.
    return picked ? { optionId: picked.optionId } : { cancelled: true };
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
      default:
        break;
    }
  }

  /** Open a picker for one pill and send the choice to the agent. */
  private async pick(key: string): Promise<void> {
    if (!this.client) {
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
      await this.client.setMode(picked.id);
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
    this.options = await this.client.setConfigOption(option.id, picked.value);
    this.pushHeader();
  }

  /** Runs a prompt to completion, signalling turnEnded either way so the
   * webview can re-enable input. */
  private async runPrompt(text: string): Promise<void> {
    try {
      await this.client?.prompt(text);
    } catch (e) {
      this.reportError(e);
    } finally {
      this.postToWebview({ type: 'turnEnded' });
    }
  }

  cancelCurrentTurn(): void {
    this.client?.cancel();
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
    ChatPanel.current = undefined;
    this.client?.dispose();
    this.panel.dispose();
    for (const d of this.disposables) {
      d.dispose();
    }
    this.disposables = [];
  }
}

function toolCallPayload(update: ToolCallUpdate) {
  return {
    toolCallId: update.toolCallId,
    title: update.title,
    status: update.status,
    kind: update.kind,
  };
}
