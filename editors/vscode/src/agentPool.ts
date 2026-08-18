import * as vscode from 'vscode';
import { AcpClient } from './acpClient';

/** Keeps one `claurst acp` process for the whole window.
 *
 * Every panel is a session inside it rather than a process of its own, so the
 * MCP servers are connected once and the model catalog is read once, however
 * many conversations are open. The process is started on the first panel and
 * stopped when the last one goes. */
export class AgentPool {
  private client: AcpClient | undefined;
  private starting: Promise<AcpClient> | undefined;
  private panels = 0;

  constructor(
    private readonly version: string,
    private readonly outputChannel: vscode.OutputChannel,
  ) {}

  /** The shared client, started if it is not running yet. */
  async acquire(cwd: string): Promise<AcpClient> {
    this.panels += 1;
    if (this.client) {
      return this.client;
    }
    if (!this.starting) {
      this.starting = this.start(cwd);
    }
    try {
      return await this.starting;
    } catch (e) {
      this.panels -= 1;
      throw e;
    }
  }

  private async start(cwd: string): Promise<AcpClient> {
    const executablePath = vscode.workspace
      .getConfiguration('claurst')
      .get<string>('executablePath', 'claurst');
    const client = new AcpClient(executablePath, cwd, this.version, {
      onStderr: (line) => this.outputChannel.appendLine(line),
      onExit: (code) => {
        this.outputChannel.appendLine(
          `[claurst-vscode] agent process exited (code ${code ?? 'unknown'})`,
        );
        // A process that died takes every session with it; the next panel
        // starts a fresh one rather than talking to a corpse.
        this.client = undefined;
        this.starting = undefined;
      },
    });
    await client.initialize();
    this.client = client;
    return client;
  }

  /** One panel is done with the agent. The last one to leave shuts it down. */
  release(): void {
    this.panels = Math.max(0, this.panels - 1);
    if (this.panels === 0) {
      this.client?.dispose();
      this.client = undefined;
      this.starting = undefined;
    }
  }

  dispose(): void {
    this.panels = 0;
    this.client?.dispose();
    this.client = undefined;
    this.starting = undefined;
  }
}
