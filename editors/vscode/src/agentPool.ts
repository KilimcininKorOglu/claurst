import * as vscode from 'vscode';
import { AcpClient } from './acpClient';

/** How many stderr lines to hold on to for a crash report. Enough for a panic
 * and its message; the output channel keeps the rest. */
const STDERR_KEPT = 20;

/** Keeps one `mikmik acp` process for the whole window.
 *
 * Every panel is a session inside it rather than a process of its own, so the
 * MCP servers are connected once and the model catalog is read once, however
 * many conversations are open. The process is started on the first panel and
 * stopped when the last one goes. */
export class AgentPool {
  private client: AcpClient | undefined;
  private starting: Promise<AcpClient> | undefined;
  private panels = 0;
  /** Told when the process goes away on its own, so panels can say so and
   * offer to start another. Without this a dead agent showed up only as
   * whatever request happened to fail next. */
  private readonly watchers = new Set<(code: number | null) => void>();
  /** The last few lines the process printed before it stopped. An exit code on
   * its own says a crash happened, not what it was. */
  private recentStderr: string[] = [];

  constructor(
    private readonly version: string,
    private readonly outputChannel: vscode.OutputChannel,
  ) {}

  /** Watch for the process dying. The returned function stops watching. */
  onDied(watcher: (code: number | null) => void): () => void {
    this.watchers.add(watcher);
    return () => this.watchers.delete(watcher);
  }

  /** What the agent said on its way out. */
  get lastOutput(): string[] {
    return [...this.recentStderr];
  }

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
    // A new process reports its own failures; the last one's are history.
    this.recentStderr = [];
    const config = vscode.workspace.getConfiguration('mikmik');
    const executablePath = config.get<string>('executablePath', 'mikmik');
    const hostTerminals = config.get<boolean>('hostTerminals', false);
    const timeoutSeconds = config.get<number>('requestTimeoutSeconds', 120);
    const client = new AcpClient(
      executablePath,
      cwd,
      this.version,
      {
        onStderr: (line) => {
          this.outputChannel.appendLine(line);
          this.recentStderr.push(line);
          if (this.recentStderr.length > STDERR_KEPT) {
            this.recentStderr.shift();
          }
        },
        onExit: (code) => {
          this.outputChannel.appendLine(
            `[mikmik-vscode] agent process exited (code ${code ?? 'unknown'})`,
          );
          // A process that died takes every session with it; the next panel
          // starts a fresh one rather than talking to a corpse.
          this.client = undefined;
          this.starting = undefined;
          for (const watcher of this.watchers) {
            watcher(code);
          }
        },
      },
      hostTerminals,
      Math.max(1, timeoutSeconds) * 1000,
    );
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
