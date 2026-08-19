import * as vscode from 'vscode';

/** What the agent is doing, as one word. */
export type AgentState = 'idle' | 'ready' | 'busy';

/** A standing indication of whether the agent is running and whether it is
 * answering.
 *
 * A turn happens in a panel that may not be the visible editor, so there was
 * no way to tell from outside it whether the agent was still working or had
 * finished a while ago. */
export class StatusBar {
  private readonly item: vscode.StatusBarItem;

  constructor() {
    this.item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
    this.item.command = 'claurst.openChat';
    this.set('idle');
    this.item.show();
  }

  set(state: AgentState): void {
    switch (state) {
      case 'busy':
        this.item.text = '$(sync~spin) Claurst';
        this.item.tooltip = 'Claurst is working. Click to open the chat.';
        return;
      case 'ready':
        this.item.text = '$(comment-discussion) Claurst';
        this.item.tooltip = 'Claurst is running. Click to open the chat.';
        return;
      default:
        this.item.text = '$(circle-outline) Claurst';
        this.item.tooltip = 'Claurst is not running. Click to start a conversation.';
    }
  }

  dispose(): void {
    this.item.dispose();
  }
}
