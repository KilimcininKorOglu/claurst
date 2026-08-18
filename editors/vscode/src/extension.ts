import * as vscode from 'vscode';
import { ChatPanel } from './chatPanel';

export function activate(context: vscode.ExtensionContext): void {
  const outputChannel = vscode.window.createOutputChannel('Claurst');
  context.subscriptions.push(outputChannel);
  // The agent is told which client it is talking to; the manifest is the one
  // place that version lives, so nothing else has to be kept in step with it.
  const version: string = context.extension.packageJSON.version;

  context.subscriptions.push(
    vscode.commands.registerCommand('claurst.openChat', () => {
      ChatPanel.createOrShow(context.extensionUri, version, outputChannel);
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('claurst.newSession', () => {
      ChatPanel.current?.dispose();
      ChatPanel.createOrShow(context.extensionUri, version, outputChannel);
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('claurst.stopSession', () => {
      ChatPanel.current?.cancelCurrentTurn();
    }),
  );
}

export function deactivate(): void {
  ChatPanel.current?.dispose();
}
