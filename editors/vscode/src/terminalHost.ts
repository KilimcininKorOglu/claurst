import * as cp from 'child_process';

/** Commands the agent asked this extension to run.
 *
 * Only used when the user turns `claurst.hostTerminals` on. The agent's own
 * path runs a command in a real PTY, which is what makes tools that check
 * `isatty` behave the way they do in a terminal; a child process here is on a
 * pipe, so that behaviour is lost. What is gained is that the command is the
 * editor's, so it can be shown live and killed with the panel.
 *
 * Output is capped per terminal. Beyond the cap the oldest bytes are dropped
 * and the reader is told, rather than the buffer growing until the extension
 * host runs out of memory. */

export type TerminalExit = { exitCode?: number; signal?: string };

export type TerminalSnapshot = {
  output: string;
  truncated: boolean;
  exit?: TerminalExit;
};

type Entry = {
  child: cp.ChildProcess;
  output: string;
  truncated: boolean;
  exit?: TerminalExit;
  /** Resolved when the child exits; awaited by wait_for_exit. */
  exited: Promise<TerminalExit>;
};

export type TerminalStart = {
  command: string;
  args: string[];
  env: Record<string, string>;
  cwd?: string;
  outputByteLimit?: number;
};

/** Default cap, matching what the agent truncates its own output to. */
const DEFAULT_LIMIT = 100_000;

export class TerminalHost {
  private terminals = new Map<string, Entry>();
  private nextId = 1;

  constructor(private readonly onData?: (terminalId: string, chunk: string) => void) {}

  create(start: TerminalStart): string {
    const id = `vscode-term-${this.nextId++}`;
    const limit = start.outputByteLimit ?? DEFAULT_LIMIT;
    const child = cp.spawn(start.command, start.args, {
      cwd: start.cwd,
      env: { ...process.env, ...start.env },
      stdio: ['ignore', 'pipe', 'pipe'],
    });

    const entry: Entry = {
      child,
      output: '',
      truncated: false,
      exited: new Promise<TerminalExit>((resolve) => {
        child.on('exit', (code, signal) => {
          const exit: TerminalExit = {
            exitCode: code ?? undefined,
            signal: signal ?? undefined,
          };
          const stored = this.terminals.get(id);
          if (stored) {
            stored.exit = exit;
          }
          resolve(exit);
        });
        child.on('error', (e) => {
          const stored = this.terminals.get(id);
          if (stored) {
            stored.output += `\n[could not run ${start.command}: ${e.message}]`;
            stored.exit = { exitCode: 127 };
          }
          resolve({ exitCode: 127 });
        });
      }),
    };

    const append = (chunk: Buffer) => {
      const text = chunk.toString('utf8');
      entry.output += text;
      if (entry.output.length > limit) {
        entry.output = entry.output.slice(entry.output.length - limit);
        entry.truncated = true;
      }
      this.onData?.(id, text);
    };
    child.stdout?.on('data', append);
    child.stderr?.on('data', append);

    this.terminals.set(id, entry);
    return id;
  }

  /** What it has said so far, and how it ended if it has. */
  snapshot(terminalId: string): TerminalSnapshot | undefined {
    const entry = this.terminals.get(terminalId);
    if (!entry) {
      return undefined;
    }
    return { output: entry.output, truncated: entry.truncated, exit: entry.exit };
  }

  async waitForExit(terminalId: string): Promise<TerminalExit | undefined> {
    const entry = this.terminals.get(terminalId);
    if (!entry) {
      return undefined;
    }
    return entry.exit ?? (await entry.exited);
  }

  /** Stop it but keep it, so its output can still be read. */
  kill(terminalId: string): boolean {
    const entry = this.terminals.get(terminalId);
    if (!entry) {
      return false;
    }
    entry.child.kill('SIGKILL');
    return true;
  }

  /** Forget it. A terminal still running is killed first: releasing one that
   * outlives the turn would leave a process nobody is watching. */
  release(terminalId: string): boolean {
    const entry = this.terminals.get(terminalId);
    if (!entry) {
      return false;
    }
    if (entry.exit === undefined) {
      entry.child.kill('SIGKILL');
    }
    this.terminals.delete(terminalId);
    return true;
  }

  disposeAll(): void {
    for (const id of [...this.terminals.keys()]) {
      this.release(id);
    }
  }
}
