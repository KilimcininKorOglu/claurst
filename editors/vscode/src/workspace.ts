import * as os from 'os';
import * as vscode from 'vscode';

/** Which folder a new conversation should work in.
 *
 * A session's cwd decides what the agent can see, which files a mention
 * resolves against, and which stored sessions `session/list` returns. In a
 * workspace with more than one root, taking the first silently pointed every
 * conversation at whichever folder happened to be listed first, and there was
 * no way to open one against another root at all.
 *
 * `undefined` means the user dismissed the question, which is a decision to
 * open nothing rather than a reason to guess. */
export async function chooseWorkingFolder(): Promise<string | undefined> {
  const roots = vscode.workspace.workspaceFolders ?? [];
  if (roots.length === 0) {
    // No folder open at all. The home directory is somewhere the agent can
    // run, and it is what the terminal falls back to.
    return os.homedir();
  }
  if (roots.length === 1) {
    return roots[0].uri.fsPath;
  }
  const picked = await vscode.window.showWorkspaceFolderPick({
    placeHolder: 'Which folder should MikMik work in?',
  });
  return picked?.uri.fsPath;
}
