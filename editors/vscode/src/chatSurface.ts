import * as vscode from 'vscode';

/** Somewhere a conversation is drawn.
 *
 * VS Code has two kinds of webview and they do not share an interface: an
 * editor panel can be revealed in a column and disposed, a sidebar view can
 * only be shown and is owned by the container it lives in. Everything the
 * conversation does to its surface is one of the five things below, so it is
 * written once against this and each kind supplies its own translation. */
export interface ChatSurface {
  readonly webview: vscode.Webview;
  title: string;
  /** Bring it to the front. */
  reveal(preserveFocus: boolean): void;
  /** Close it, where closing is a thing that can be done. */
  dispose(): void;
  onDidDispose(listener: () => void): vscode.Disposable;
  /** The user is looking at this one, so palette commands should act on it. */
  onDidBecomeActive(listener: () => void): vscode.Disposable;
}

/** A conversation in an editor tab. */
export class PanelSurface implements ChatSurface {
  constructor(private readonly panel: vscode.WebviewPanel) {}

  get webview(): vscode.Webview {
    return this.panel.webview;
  }

  get title(): string {
    return this.panel.title;
  }

  set title(value: string) {
    this.panel.title = value;
  }

  reveal(preserveFocus: boolean): void {
    this.panel.reveal(undefined, preserveFocus);
  }

  dispose(): void {
    this.panel.dispose();
  }

  onDidDispose(listener: () => void): vscode.Disposable {
    return this.panel.onDidDispose(listener);
  }

  onDidBecomeActive(listener: () => void): vscode.Disposable {
    return this.panel.onDidChangeViewState(() => {
      if (this.panel.active) {
        listener();
      }
    });
  }
}

/** A conversation in the sidebar. */
export class ViewSurface implements ChatSurface {
  /** The view's own title sits under the container's, and setting it to a
   * conversation's name is how the sidebar says which one is open. */
  constructor(private readonly view: vscode.WebviewView) {}

  get webview(): vscode.Webview {
    return this.view.webview;
  }

  get title(): string {
    return this.view.title ?? 'MikMik';
  }

  set title(value: string) {
    this.view.title = value;
  }

  reveal(preserveFocus: boolean): void {
    this.view.show(preserveFocus);
  }

  dispose(): void {
    // A view belongs to its container. There is no closing it from here, and
    // pretending otherwise would leave the conversation running behind a view
    // that looked shut.
  }

  onDidDispose(listener: () => void): vscode.Disposable {
    return this.view.onDidDispose(listener);
  }

  onDidBecomeActive(listener: () => void): vscode.Disposable {
    return this.view.onDidChangeVisibility(() => {
      if (this.view.visible) {
        listener();
      }
    });
  }
}
