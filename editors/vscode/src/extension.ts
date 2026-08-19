import * as vscode from 'vscode';
import { AgentPool } from './agentPool';
import { ChatPanel } from './chatPanel';
import { chooseWorkingFolder } from './workspace';

export function activate(context: vscode.ExtensionContext): void {
  const outputChannel = vscode.window.createOutputChannel('Claurst');
  context.subscriptions.push(outputChannel);
  // The agent is told which client it is talking to; the manifest is the one
  // place that version lives, so nothing else has to be kept in step with it.
  const version: string = context.extension.packageJSON.version;
  const pool = new AgentPool(version, outputChannel);
  context.subscriptions.push({ dispose: () => pool.dispose() });
  context.subscriptions.push(watchConfiguration(pool));

  context.subscriptions.push(
    vscode.commands.registerCommand('claurst.openChat', async () => {
      await ChatPanel.show(context.extensionUri, pool, outputChannel);
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('claurst.newSession', async () => {
      // A second panel, not a replacement: two conversations can run side by
      // side inside the one agent process.
      const cwd = await chooseWorkingFolder();
      if (cwd) {
        ChatPanel.create(context.extensionUri, pool, outputChannel, { kind: 'new', cwd });
      }
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('claurst.stopSession', () => {
      ChatPanel.active?.cancelCurrentTurn();
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('claurst.forkSession', () => {
      // Trying a second approach without losing the first: the fork carries
      // the conversation so far, and the original is untouched.
      const source = ChatPanel.active?.session;
      if (!source) {
        vscode.window.showInformationMessage('No Claurst conversation to fork.');
        return;
      }
      ChatPanel.create(context.extensionUri, pool, outputChannel, {
        kind: 'fork',
        sessionId: source.sessionId,
        cwd: source.cwd,
        title: source.title,
      });
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('claurst.resumeSession', async () => {
      try {
        const cwd = await chooseWorkingFolder();
        if (!cwd) {
          return;
        }
        const client = await pool.acquire(cwd);
        try {
          const sessions = await client.listSessions(cwd);
          if (sessions.length === 0) {
            vscode.window.showInformationMessage('No earlier sessions in this folder.');
            return;
          }
          const picked = await vscode.window.showQuickPick(
            sessions.map((session) => ({
              label: session.title ?? session.sessionId,
              description: session.updatedAt,
              session,
            })),
            { placeHolder: 'Which conversation should Claurst pick up?' },
          );
          if (!picked) {
            return;
          }
          // Replaying a long conversation costs a message per block, so it is
          // asked for rather than assumed.
          // `kind` on a quick-pick item means a separator, so the choice
          // travels under a name of its own.
          const how = await vscode.window.showQuickPick(
            [
              { label: 'Show the conversation', opening: 'load' as const },
              { label: 'Just carry on', opening: 'resume' as const },
            ],
            { placeHolder: 'Draw what was said before?' },
          );
          if (how) {
            const panel = ChatPanel.create(context.extensionUri, pool, outputChannel, {
              kind: how.opening,
              session: picked.session,
            });
            // Wait for the panel to take its own share before giving this one
            // back. Releasing first drops the count to zero, which stops the
            // process the panel is about to ask for and starts another.
            await panel.started;
          }
        } finally {
          // The listing borrowed the agent; only a panel keeps it running.
          pool.release();
        }
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        vscode.window.showErrorMessage(`Claurst: ${message}`);
      }
    }),
  );
}

/** Tell the user when a setting they changed will not take effect yet.
 *
 * These three are read once, when the agent process starts. Changing one and
 * seeing nothing happen reads as the setting not working, so the offer to
 * restart is made where the change was made. */
function watchConfiguration(pool: AgentPool): vscode.Disposable {
  const startupSettings = [
    'claurst.executablePath',
    'claurst.hostTerminals',
    'claurst.requestTimeoutSeconds',
  ];
  return vscode.workspace.onDidChangeConfiguration(async (event) => {
    if (!startupSettings.some((setting) => event.affectsConfiguration(setting))) {
      return;
    }
    const restart = 'Restart agent';
    const answer = await vscode.window.showInformationMessage(
      'Claurst: this setting applies when the agent starts. Restart it now?',
      restart,
    );
    if (answer !== restart) {
      return;
    }
    // Every open panel is a session inside the process, so they go with it.
    // Reopening one against a half-dead process would fail later and less
    // clearly than closing them here.
    ChatPanel.disposeAll();
    pool.dispose();
  });
}

export function deactivate(): void {
  ChatPanel.disposeAll();
}
