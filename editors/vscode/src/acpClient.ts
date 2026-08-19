import * as cp from 'child_process';
import * as readline from 'readline';
import * as vscode from 'vscode';
import { TerminalHost } from './terminalHost';

/** Minimal newline-delimited JSON-RPC 2.0 client for the Agent Client Protocol,
 * matching the wire format implemented in src-rust/crates/acp/src/connection.rs:
 * one UTF-8 line per message, no Content-Length framing. */

export type PermissionOption = {
  optionId: string;
  name: string;
  kind: string;
};

/** A file a tool rewrote, as the agent reported it. */
export type ToolDiff = {
  path: string;
  oldText?: string;
  newText: string;
};

/** A place in the workspace a tool call is about. */
export type ToolCallLocation = {
  path: string;
  /** 1-based, when the agent knows which line. */
  line?: number;
};

export type ToolCallUpdate = {
  toolCallId?: string;
  title?: string;
  status?: string;
  kind?: string;
  /** The files this call touches, so the panel can offer to open them. */
  locations: ToolCallLocation[];
  /** The text the agent attached to this call: what the tool returned once it
   * ran, or what the call is about to do when it is asking to be approved. */
  output?: string;
  /** Every file this tool rewrote. */
  diffs: ToolDiff[];
  /** The terminal this call is running in, when we are hosting it. */
  terminalId?: string;
};

/** One step of the agent's plan. */
export type PlanEntry = {
  content: string;
  status: string;
  priority?: string;
};

/** One value a select-style configuration option can be set to. */
export type ConfigValue = {
  value: string;
  name: string;
};

/** A model / account / reasoning-level selector the agent offers. */
export type ConfigOption = {
  id: string;
  name: string;
  category?: string;
  currentValue: string;
  values: ConfigValue[];
};

export type SessionMode = {
  id: string;
  name: string;
  description?: string;
};

export type SessionModes = {
  currentModeId: string;
  availableModes: SessionMode[];
};

/** A slash command the agent offers. */
export type AvailableCommand = {
  name: string;
  description: string;
  hint?: string;
};

/** What `session/new`, `session/load` or `session/resume` told us. */
export type SessionStart = {
  sessionId: string;
  modes?: SessionModes;
  configOptions: ConfigOption[];
};

/** One session on file, as `session/list` describes it. */
export type StoredSession = {
  sessionId: string;
  cwd: string;
  title?: string;
  updatedAt?: string;
};

/** A block of a prompt: plain text, an image, a named file, or a file's
 * contents. The agent declares `prompt_capabilities.image`, so an image
 * reaches the model rather than being described to it. */
export type PromptBlock =
  | { type: 'text'; text: string }
  | { type: 'image'; mimeType: string; data: string }
  | { type: 'resource_link'; uri: string; name: string }
  | { type: 'resource'; resource: { uri: string; text: string; mimeType?: string } };

/** How the user answered a permission request, or that they answered nothing. */
export type PermissionAnswer = { optionId: string } | { cancelled: true };

/** Who a streamed chunk came from. A replayed transcript carries the user's
 * own turns, which are not the agent talking. */
export type ChunkKind = 'agent' | 'thought' | 'user';

/** What one session's panel wants to hear about. */
export interface SessionEvents {
  onTextChunk?: (text: string, kind: ChunkKind) => void;
  /** An image in a streamed or replayed turn, as base64 with its media type. */
  onImage?: (mimeType: string, data: string, kind: ChunkKind) => void;
  onToolCall?: (update: ToolCallUpdate) => void;
  onToolCallUpdate?: (update: ToolCallUpdate) => void;
  /** Resolves to a chosen option id, or to `{cancelled: true}` when the user
   * dismissed the prompt without choosing. */
  onRequestPermission?: (
    toolCall: ToolCallUpdate,
    options: PermissionOption[],
  ) => Promise<PermissionAnswer>;
  /** The agent restated the whole option set after one of them changed. */
  onConfigOptions?: (options: ConfigOption[]) => void;
  /** The session's mode changed, for whatever reason. */
  onModeChanged?: (modeId: string) => void;
  /** The commands this session can run. */
  onCommands?: (commands: AvailableCommand[]) => void;
  /** The agent's plan, restated in full each time it moves. */
  onPlan?: (entries: PlanEntry[]) => void;
  /** The session was named, or renamed. */
  onSessionInfo?: (title?: string) => void;
  /** More output from a terminal this extension is hosting. */
  onTerminalOutput?: (terminalId: string, chunk: string) => void;
}

export interface AcpClientEvents {
  onStderr?: (line: string) => void;
  onExit?: (code: number | null) => void;
}

/** Speaks ACP to a `claurst acp` child process over stdio.
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
      this.events.onStderr?.(`[claurst-vscode] ${e.message}`);
    });
    this.child.on('exit', (code) => {
      for (const { reject } of this.pending.values()) {
        reject(new Error('claurst acp process exited'));
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
      this.events.onStderr?.(`[claurst-vscode] malformed line from agent: ${trimmed}`);
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
        this.events.onStderr?.(`[claurst-vscode] failed to handle ${msg.method}: ${e}`);
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
        clientInfo: { name: 'claurst-vscode', version: this.clientVersion },
      }).then(() => undefined);
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

function startOf(result: any, sessionId: string): SessionStart {
  return {
    sessionId,
    modes: result?.modes,
    configOptions: parseConfigOptions(result?.configOptions),
  };
}

function toolCallOf(update: any): ToolCallUpdate {
  const content: any[] = Array.isArray(update.content) ? update.content : [];
  const diffs: ToolDiff[] = content
    .filter((block) => block?.type === 'diff')
    .map((block) => ({
      path: block.path,
      oldText: block.oldText ?? undefined,
      newText: block.newText ?? '',
    }));
  const output = content
    .filter((block) => block?.type === 'content')
    .map((block) => extractText(block.content))
    .filter((text) => text.length > 0)
    .join('\n');
  const terminal = content.find((block) => block?.type === 'terminal');
  return {
    toolCallId: update.toolCallId,
    title: update.title,
    status: update.status,
    kind: update.kind,
    locations: locationsOf(update.locations),
    output: output.length > 0 ? output : undefined,
    diffs,
    terminalId: terminal?.terminalId ?? undefined,
  };
}

/** Where the agent said the call is working.
 *
 * A location without a path is dropped rather than kept as a blank entry: the
 * panel turns each one into something to click, and there is nothing to open. */
function locationsOf(raw: any): ToolCallLocation[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  return raw
    .filter((location) => typeof location?.path === 'string' && location.path.length > 0)
    .map((location) => ({
      path: location.path,
      line: typeof location.line === 'number' ? location.line : undefined,
    }));
}

/** Hand one content block to whichever event can carry it.
 *
 * Every block shape the agent can send is named here. A block that only
 * `extractText` understood used to arrive as an empty string, which drew a
 * blank bubble: a replayed turn with an attached file showed nothing where the
 * attachment was, and an image showed nothing at all. */
function deliver(handler: SessionEvents, content: any, kind: ChunkKind): void {
  switch (content?.type) {
    case 'text':
      handler.onTextChunk?.(content.text ?? '', kind);
      return;
    case 'image':
      if (typeof content.data === 'string' && content.data.length > 0) {
        handler.onImage?.(content.mimeType ?? 'image/png', content.data, kind);
      }
      return;
    case 'resource_link':
      handler.onTextChunk?.(`@${content.name ?? content.uri ?? ''}`, kind);
      return;
    case 'resource':
      // The contents travelled with the prompt and are already in the model's
      // view. Replaying them into the panel would bury the turn that carried
      // them, so the attachment is named instead.
      handler.onTextChunk?.(`@${content.resource?.uri ?? ''}`, kind);
      return;
    default:
      return;
  }
}

function extractText(content: any): string {
  if (content?.type === 'text') {
    return content.text ?? '';
  }
  return '';
}

function parseCommands(raw: any): AvailableCommand[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  return raw
    .filter((command) => typeof command?.name === 'string')
    .map((command) => ({
      name: command.name,
      description: command.description ?? '',
      hint: command.input?.hint ?? undefined,
    }));
}

function parsePlan(raw: any): PlanEntry[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  return raw.map((entry) => ({
    content: entry?.content ?? '',
    status: entry?.status ?? 'pending',
    priority: entry?.priority ?? undefined,
  }));
}

/** Flatten the protocol's select options into what the panel renders.
 *
 * Only `select` options are understood; anything else is dropped rather than
 * shown as an empty picker. */
function parseConfigOptions(raw: any): ConfigOption[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  const parsed: ConfigOption[] = [];
  for (const option of raw) {
    if (option?.type !== 'select') {
      continue;
    }
    // `options` is either a flat list or a list of groups; the agent sends a
    // flat one, and a grouped one is flattened rather than ignored.
    const flat: any[] = Array.isArray(option.options)
      ? option.options.flatMap((entry: any) => (Array.isArray(entry?.options) ? entry.options : [entry]))
      : [];
    parsed.push({
      id: option.id,
      name: option.name ?? option.id,
      category: option.category,
      currentValue: option.currentValue,
      values: flat.map((value: any) => ({ value: value.value, name: value.name ?? value.value })),
    });
  }
  return parsed;
}
