import * as cp from 'child_process';
import * as readline from 'readline';
import * as vscode from 'vscode';
import {
  AgentCapabilities,
  ChunkKind,
  ConfigOption,
  PermissionOption,
  PromptBlock,
  SessionEvents,
  SessionStart,
  StoredSession,
  ToolCallUpdate,
  capabilitiesOf,
  deliver,
  parseCommands,
  parseConfigOptions,
  parsePlan,
  startOf,
  toolCallOf,
} from './protocol';
import { TerminalHost } from './terminalHost';

// Re-exported so callers keep one import for the client and the shapes it
// speaks in.
export * from './protocol';

/** Minimal newline-delimited JSON-RPC 2.0 client for the Agent Client Protocol,
 * matching the wire format implemented in src-rust/crates/acp/src/connection.rs:
 * one UTF-8 line per message, no Content-Length framing. */

export interface AcpClientEvents {
  onStderr?: (line: string) => void;
  onExit?: (code: number | null) => void;
}

/** Speaks ACP to a `mikmik acp` child process over stdio.
 *
 * One process serves every panel: sessions are independent inside it, and
 * they share the MCP connections and the model catalog the agent built once
 * at startup. Each update carries the session it belongs to, which is how it
 * reaches the right panel. */
export class AcpClient {
  private child: cp.ChildProcessWithoutNullStreams;
  private rl: readline.Interface;
  private nextId = 1;
  private pending = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>();
  private sessions = new Map<string, SessionEvents>();
  private initialized: Promise<void> | undefined;
  private agentCapabilities: AgentCapabilities = {
    image: false,
    embeddedContext: false,
    loadSession: false,
  };
  private readonly terminals: TerminalHost;
  /** Which session started each terminal, so its output reaches that panel. */
  private terminalOwners = new Map<string, string>();

  constructor(
    executablePath: string,
    cwd: string,
    private clientVersion: string,
    private events: AcpClientEvents,
    /** Whether to run the agent's commands here rather than in the agent. Off
     * by default: the agent runs them in a real PTY, and a child process here
     * is on a pipe, so anything that checks `isatty` behaves differently. */
    private readonly hostTerminals = false,
    /** How long to wait for an answer before giving up on a request. Does not
     * apply to `session/prompt`; see `request`. */
    private readonly timeoutMs = 120_000,
  ) {
    this.terminals = new TerminalHost((terminalId, chunk) => {
      const sessionId = this.terminalOwners.get(terminalId);
      const handler = sessionId ? this.sessions.get(sessionId) : undefined;
      handler?.onTerminalOutput?.(terminalId, chunk);
    });
    this.child = cp.spawn(executablePath, ['acp'], { cwd, stdio: ['pipe', 'pipe', 'pipe'] });
    this.rl = readline.createInterface({ input: this.child.stdout });
    this.rl.on('line', (line) => this.handleLine(line));
    this.child.stderr.on('data', (data: Buffer) => {
      const text = data.toString('utf8');
      for (const line of text.split('\n')) {
        if (line.trim().length > 0) {
          this.events.onStderr?.(line);
        }
      }
    });
    this.child.on('error', (e) => {
      // A missing binary lands here, not on stdout, so surface it as a failed
      // request rather than leaving the panel waiting forever.
      for (const { reject } of this.pending.values()) {
        reject(new Error(`could not run '${executablePath}': ${e.message}`));
      }
      this.pending.clear();
      this.events.onStderr?.(`[mikmik-vscode] ${e.message}`);
    });
    this.child.on('exit', (code) => {
      for (const { reject } of this.pending.values()) {
        reject(new Error('mikmik acp process exited'));
      }
      this.pending.clear();
      this.events.onExit?.(code);
    });
  }

  private handleLine(line: string): void {
    const trimmed = line.trim();
    if (trimmed.length === 0) {
      return;
    }
    let msg: any;
    try {
      msg = JSON.parse(trimmed);
    } catch {
      this.events.onStderr?.(`[mikmik-vscode] malformed line from agent: ${trimmed}`);
      return;
    }

    const hasId = msg.id !== undefined && msg.id !== null;
    const hasResult = 'result' in msg;
    const hasError = 'error' in msg;
    const hasMethod = typeof msg.method === 'string';

    if (hasId && (hasResult || hasError) && !hasMethod) {
      const pending = this.pending.get(msg.id);
      if (!pending) {
        return;
      }
      this.pending.delete(msg.id);
      if (hasError) {
        pending.reject(
          Object.assign(new Error(msg.error?.message ?? 'ACP error'), { data: msg.error }),
        );
      } else {
        pending.resolve(msg.result);
      }
      return;
    }

    if (hasId && hasMethod) {
      // Agent → client request. Only session/request_permission is expected.
      this.handleIncomingRequest(msg.id, msg.method, msg.params).catch((e) => {
        this.events.onStderr?.(`[mikmik-vscode] failed to handle ${msg.method}: ${e}`);
      });
      return;
    }

    if (hasMethod) {
      this.handleNotification(msg.method, msg.params);
    }
  }

  private async handleIncomingRequest(id: number, method: string, params: any): Promise<void> {
    if (method === 'session/request_permission') {
      const handler = this.sessions.get(params?.sessionId);
      // The agent describes what it is about to do in the same shape it
      // describes what it did: a diff for a whole-file write, text for
      // everything else. Rebuilding the call by hand here used to drop all of
      // it, which asked the user to approve something they could not see.
      const toolCall = toolCallOf(params?.toolCall ?? {});
      const options: PermissionOption[] = (params?.options ?? []).map((o: any) => ({
        optionId: o.optionId,
        name: o.name,
        kind: o.kind,
      }));
      const answer =
        (await handler?.onRequestPermission?.(toolCall, options)) ??
        ({ cancelled: true } as const);
      // Dismissing the prompt means the user chose nothing. Falling back to
      // the first offered option would run the tool they walked away from,
      // because the first option is "allow once".
      const outcome =
        'optionId' in answer
          ? { outcome: 'selected', optionId: answer.optionId }
          : { outcome: 'cancelled' };
      this.writeMessage({ jsonrpc: '2.0', id, result: { outcome } });
      return;
    }

    if (method === 'fs/read_text_file') {
      try {
        this.writeMessage({
          jsonrpc: '2.0',
          id,
          result: { content: await readTextFile(params?.path, params?.line, params?.limit) },
        });
      } catch (e) {
        this.writeMessage({
          jsonrpc: '2.0',
          id,
          error: { code: -32603, message: `cannot read ${params?.path}: ${e}` },
        });
      }
      return;
    }

    if (method === 'fs/write_text_file') {
      try {
        await writeTextFile(params?.path, params?.content ?? '');
        this.writeMessage({ jsonrpc: '2.0', id, result: {} });
      } catch (e) {
        this.writeMessage({
          jsonrpc: '2.0',
          id,
          error: { code: -32603, message: `cannot write ${params?.path}: ${e}` },
        });
      }
      return;
    }

    if (method.startsWith('terminal/')) {
      await this.handleTerminalRequest(id, method, params);
      return;
    }

    // Unknown incoming request — respond with method-not-found so the agent
    // doesn't hang waiting for a reply.
    this.writeMessage({
      jsonrpc: '2.0',
      id,
      error: { code: -32601, message: `client does not implement '${method}'` },
    });
  }

  /** Run and report on the commands the agent handed us.
   *
   * A terminal id the host does not know is an error rather than an empty
   * answer: the agent would otherwise wait for output that will never come. */
  private async handleTerminalRequest(id: number, method: string, params: any): Promise<void> {
    const fail = (message: string) =>
      this.writeMessage({ jsonrpc: '2.0', id, error: { code: -32602, message } });
    const terminalId: string | undefined = params?.terminalId;

    switch (method) {
      case 'terminal/create': {
        try {
          const created = this.terminals.create({
            command: params?.command ?? '',
            args: Array.isArray(params?.args) ? params.args : [],
            env: Object.fromEntries(
              (Array.isArray(params?.env) ? params.env : []).map((v: any) => [v?.name, v?.value]),
            ),
            cwd: params?.cwd ?? undefined,
            outputByteLimit: params?.outputByteLimit ?? undefined,
          });
          if (params?.sessionId) {
            this.terminalOwners.set(created, params.sessionId);
          }
          this.writeMessage({ jsonrpc: '2.0', id, result: { terminalId: created } });
        } catch (e) {
          fail(`could not start ${params?.command}: ${e}`);
        }
        return;
      }
      case 'terminal/output': {
        const snapshot = terminalId ? this.terminals.snapshot(terminalId) : undefined;
        if (!snapshot) {
          fail(`unknown terminal ${terminalId}`);
          return;
        }
        this.writeMessage({
          jsonrpc: '2.0',
          id,
          result: {
            output: snapshot.output,
            truncated: snapshot.truncated,
            exitStatus: snapshot.exit
              ? { exitCode: snapshot.exit.exitCode, signal: snapshot.exit.signal }
              : null,
          },
        });
        return;
      }
      case 'terminal/wait_for_exit': {
        const exit = terminalId ? await this.terminals.waitForExit(terminalId) : undefined;
        if (!exit) {
          fail(`unknown terminal ${terminalId}`);
          return;
        }
        this.writeMessage({
          jsonrpc: '2.0',
          id,
          result: { exitStatus: { exitCode: exit.exitCode, signal: exit.signal } },
        });
        return;
      }
      case 'terminal/kill': {
        if (!terminalId || !this.terminals.kill(terminalId)) {
          fail(`unknown terminal ${terminalId}`);
          return;
        }
        this.writeMessage({ jsonrpc: '2.0', id, result: {} });
        return;
      }
      case 'terminal/release': {
        if (terminalId) {
          this.terminals.release(terminalId);
          this.terminalOwners.delete(terminalId);
        }
        // Releasing one that is already gone is not a failure: it is the
        // state the caller wanted.
        this.writeMessage({ jsonrpc: '2.0', id, result: {} });
        return;
      }
      default:
        this.writeMessage({
          jsonrpc: '2.0',
          id,
          error: { code: -32601, message: `client does not implement '${method}'` },
        });
    }
  }

  private handleNotification(method: string, params: any): void {
    if (method !== 'session/update') {
      return;
    }
    const update = params?.update;
    const handler = this.sessions.get(params?.sessionId);
    if (!update || !handler) {
      return;
    }
    switch (update.sessionUpdate) {
      case 'user_message_chunk':
        // Only a replayed transcript carries these, and drawing them as the
        // agent would put the user's own words in its mouth.
        deliver(handler, update.content, 'user');
        break;
      case 'agent_message_chunk':
        deliver(handler, update.content, 'agent');
        break;
      case 'agent_thought_chunk':
        deliver(handler, update.content, 'thought');
        break;
      case 'tool_call':
        handler.onToolCall?.(toolCallOf(update));
        break;
      case 'tool_call_update':
        handler.onToolCallUpdate?.(toolCallOf(update));
        break;
      case 'config_option_update':
        handler.onConfigOptions?.(parseConfigOptions(update.configOptions));
        break;
      case 'current_mode_update':
        handler.onModeChanged?.(update.currentModeId);
        break;
      case 'available_commands_update':
        handler.onCommands?.(parseCommands(update.availableCommands));
        break;
      case 'plan':
        handler.onPlan?.(parsePlan(update.entries));
        break;
      case 'session_info_update':
        handler.onSessionInfo?.(update.title ?? undefined);
        break;
      default:
        break;
    }
  }

  private writeMessage(msg: unknown): void {
    this.child.stdin.write(JSON.stringify(msg) + '\n');
  }

  /** Send a request and wait for its answer.
   *
   * An agent that stops answering used to leave the promise pending forever:
   * the panel span, the session never opened, and nothing said why. The
   * deadline turns that into a failure the caller can report.
   *
   * `session/prompt` passes `false`, and it is the only one that may. A turn
   * runs for as long as the model and its tools take, so any deadline here
   * would abandon work that is still going. The user ends it with Stop. */
  private request<T = any>(method: string, params: unknown, deadline = true): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      let timer: NodeJS.Timeout | undefined;
      const settle = {
        resolve: (value: T) => {
          clearTimeout(timer);
          resolve(value);
        },
        reject: (e: Error) => {
          clearTimeout(timer);
          reject(e);
        },
      };
      this.pending.set(id, settle);
      if (deadline) {
        timer = setTimeout(() => {
          this.pending.delete(id);
          reject(new Error(`the agent did not answer '${method}' within ${this.timeoutMs}ms`));
        }, this.timeoutMs);
      }
      this.writeMessage({ jsonrpc: '2.0', id, method, params });
    });
  }

  private notify(method: string, params: unknown): void {
    this.writeMessage({ jsonrpc: '2.0', method, params });
  }

  /** What the agent answered `initialize` with. Empty until it has. */
  get agent(): AgentCapabilities {
    return this.agentCapabilities;
  }

  /** Negotiate capabilities once, however many panels ask for it. */
  initialize(): Promise<void> {
    if (!this.initialized) {
      this.initialized = this.request('initialize', {
        protocolVersion: 1,
        // We host the files: a read sees the buffer the user is looking at,
        // including edits they have not saved, and a write goes through the
        // workspace so it joins the undo stack instead of appearing beneath
        // it. Terminals only when the user asked for it — see `hostTerminals`.
        clientCapabilities: {
          fs: { readTextFile: true, writeTextFile: true },
          terminal: this.hostTerminals,
        },
        clientInfo: { name: 'mikmik-vscode', version: this.clientVersion },
      }).then((result) => {
        this.agentCapabilities = capabilitiesOf(result);
      });
    }
    return this.initialized;
  }

  /** Start a session and route its updates to `events`. */
  async newSession(cwd: string, events: SessionEvents): Promise<SessionStart> {
    const result = await this.request<any>('session/new', { cwd, mcpServers: [] });
    this.sessions.set(result.sessionId, events);
    return startOf(result, result.sessionId);
  }

  /** Reopen a stored session. The agent replays the whole transcript as
   * updates before it answers, so the handler is registered first. */
  async loadSession(sessionId: string, cwd: string, events: SessionEvents): Promise<SessionStart> {
    this.sessions.set(sessionId, events);
    try {
      const result = await this.request<any>('session/load', { sessionId, cwd, mcpServers: [] });
      return startOf(result, sessionId);
    } catch (e) {
      this.sessions.delete(sessionId);
      throw e;
    }
  }

  /** Reopen a stored session without replaying it.
   *
   * For a client that already knows what was said, or a user who wants to keep
   * going rather than re-read the conversation. */
  async resumeSession(
    sessionId: string,
    cwd: string,
    events: SessionEvents,
  ): Promise<SessionStart> {
    this.sessions.set(sessionId, events);
    try {
      const result = await this.request<any>('session/resume', { sessionId, cwd, mcpServers: [] });
      return startOf(result, sessionId);
    } catch (e) {
      this.sessions.delete(sessionId);
      throw e;
    }
  }

  /** Split a conversation in two, leaving the original untouched.
   *
   * The fork carries the transcript so far and the choices the source made,
   * under a new id the caller routes to its own panel. */
  async forkSession(
    sessionId: string,
    cwd: string,
    events: SessionEvents,
  ): Promise<SessionStart> {
    const result = await this.request<any>('session/fork', { sessionId, cwd, mcpServers: [] });
    this.sessions.set(result.sessionId, events);
    return startOf(result, result.sessionId);
  }

  /** Every session on file, newest first. */
  async listSessions(cwd?: string): Promise<StoredSession[]> {
    const result = await this.request<any>('session/list', cwd ? { cwd } : {});
    const sessions: any[] = Array.isArray(result?.sessions) ? result.sessions : [];
    return sessions.map((s) => ({
      sessionId: s.sessionId,
      cwd: s.cwd,
      title: s.title ?? undefined,
      updatedAt: s.updatedAt ?? undefined,
    }));
  }

  async prompt(sessionId: string, blocks: PromptBlock[]): Promise<void> {
    await this.request('session/prompt', { sessionId, prompt: blocks }, false);
  }

  /** Change the model, the account, or the reasoning effort. Returns the whole
   * option set, since changing one of them can change the others' values. */
  async setConfigOption(
    sessionId: string,
    configId: string,
    value: string,
  ): Promise<ConfigOption[]> {
    const result = await this.request<any>('session/set_config_option', {
      sessionId,
      configId,
      value,
    });
    return parseConfigOptions(result?.configOptions);
  }

  async setMode(sessionId: string, modeId: string): Promise<void> {
    await this.request('session/set_mode', { sessionId, modeId });
  }

  cancel(sessionId: string): void {
    this.notify('session/cancel', { sessionId });
  }

  /** Let the agent go of a session, and stop routing its updates. */
  async closeSession(sessionId: string): Promise<void> {
    this.sessions.delete(sessionId);
    try {
      await this.request('session/close', { sessionId });
    } catch {
      // A session the agent has already dropped needs no closing, and the
      // panel is going away either way.
    }
  }

  get sessionCount(): number {
    return this.sessions.size;
  }

  dispose(): void {
    this.terminals.disposeAll();
    this.rl.close();
    this.child.kill();
  }
}

/** The file as the editor has it, not as the disk has it.
 *
 * An open document is read from the editor's own copy, so unsaved edits are
 * what the agent sees; anything else is read from the workspace filesystem,
 * which also covers a remote workspace where the agent's own disk is not the
 * one holding the file.
 *
 * `line` is 1-based and `limit` counts lines, matching the protocol. */
async function readTextFile(path: string, line?: number, limit?: number): Promise<string> {
  if (typeof path !== 'string' || path.length === 0) {
    throw new Error('no path was given');
  }
  const uri = vscode.Uri.file(path);
  const open = vscode.workspace.textDocuments.find((doc) => doc.uri.fsPath === uri.fsPath);
  const text = open
    ? open.getText()
    : Buffer.from(await vscode.workspace.fs.readFile(uri)).toString('utf8');

  if (line === undefined && limit === undefined) {
    return text;
  }
  const lines = text.split('\n');
  const from = Math.max(0, (line ?? 1) - 1);
  const to = limit === undefined ? lines.length : from + limit;
  return lines.slice(from, to).join('\n');
}

/** Write through a workspace edit, so the change is undoable and shows up in
 * the editor the user is looking at rather than underneath it.
 *
 * A file that does not exist yet is created first: a workspace edit cannot
 * replace a range in a document there is none of. */
async function writeTextFile(path: string, content: string): Promise<void> {
  if (typeof path !== 'string' || path.length === 0) {
    throw new Error('no path was given');
  }
  const uri = vscode.Uri.file(path);
  try {
    await vscode.workspace.fs.stat(uri);
  } catch {
    await vscode.workspace.fs.writeFile(uri, Buffer.from(content, 'utf8'));
    return;
  }

  const doc = await vscode.workspace.openTextDocument(uri);
  const edit = new vscode.WorkspaceEdit();
  const whole = new vscode.Range(
    doc.lineAt(0).range.start,
    doc.lineAt(doc.lineCount - 1).range.end,
  );
  edit.replace(uri, whole, content);
  if (!(await vscode.workspace.applyEdit(edit))) {
    throw new Error('the workspace refused the edit');
  }
}
