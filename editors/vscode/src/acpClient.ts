import * as cp from 'child_process';
import * as readline from 'readline';
import * as vscode from 'vscode';

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

export type ToolCallUpdate = {
  toolCallId?: string;
  title?: string;
  status?: string;
  kind?: string;
  /** The text the tool returned, when it returned any. */
  output?: string;
  /** Every file this tool rewrote. */
  diffs: ToolDiff[];
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

/** A block of a prompt: plain text, a named file, or a file's contents. */
export type PromptBlock =
  | { type: 'text'; text: string }
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

  constructor(
    executablePath: string,
    cwd: string,
    private clientVersion: string,
    private events: AcpClientEvents,
  ) {
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
      const toolCall: ToolCallUpdate = {
        toolCallId: params?.toolCall?.toolCallId,
        title: params?.toolCall?.title,
        status: params?.toolCall?.status,
        kind: params?.toolCall?.kind,
        diffs: [],
      };
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

    // Unknown incoming request — respond with method-not-found so the agent
    // doesn't hang waiting for a reply.
    this.writeMessage({
      jsonrpc: '2.0',
      id,
      error: { code: -32601, message: `client does not implement '${method}'` },
    });
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
        handler.onTextChunk?.(extractText(update.content), 'user');
        break;
      case 'agent_message_chunk':
        handler.onTextChunk?.(extractText(update.content), 'agent');
        break;
      case 'agent_thought_chunk':
        handler.onTextChunk?.(extractText(update.content), 'thought');
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

  private request<T = any>(method: string, params: unknown): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
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
        // it. Terminals stay with the agent, which already runs commands in a
        // PTY and reports their output.
        clientCapabilities: {
          fs: { readTextFile: true, writeTextFile: true },
          terminal: false,
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
    await this.request('session/prompt', { sessionId, prompt: blocks });
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
  return {
    toolCallId: update.toolCallId,
    title: update.title,
    status: update.status,
    kind: update.kind,
    output: output.length > 0 ? output : undefined,
    diffs,
  };
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
