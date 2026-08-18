import * as cp from 'child_process';
import * as readline from 'readline';

/** Minimal newline-delimited JSON-RPC 2.0 client for the Agent Client Protocol,
 * matching the wire format implemented in src-rust/crates/acp/src/connection.rs:
 * one UTF-8 line per message, no Content-Length framing. */

export type PermissionOption = {
  optionId: string;
  name: string;
  kind: string;
};

export type ToolCallUpdate = {
  toolCallId?: string;
  title?: string;
  status?: string;
  kind?: string;
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

/** What `session/new` told us about the session it just created. */
export type SessionStart = {
  sessionId: string;
  modes?: SessionModes;
  configOptions: ConfigOption[];
};

/** How the user answered a permission request, or that they answered nothing. */
export type PermissionAnswer = { optionId: string } | { cancelled: true };

export interface AcpClientEvents {
  onTextChunk?: (text: string, isThought: boolean) => void;
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
  onStderr?: (line: string) => void;
  onExit?: (code: number | null) => void;
}

/** Speaks ACP to a `claurst acp` child process over stdio. */
export class AcpClient {
  private child: cp.ChildProcessWithoutNullStreams;
  private rl: readline.Interface;
  private nextId = 1;
  private pending = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>();
  private sessionId: string | undefined;

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
      const toolCall: ToolCallUpdate = {
        toolCallId: params?.toolCall?.toolCallId,
        title: params?.toolCall?.title,
        status: params?.toolCall?.status,
        kind: params?.toolCall?.kind,
      };
      const options: PermissionOption[] = (params?.options ?? []).map((o: any) => ({
        optionId: o.optionId,
        name: o.name,
        kind: o.kind,
      }));
      const answer =
        (await this.events.onRequestPermission?.(toolCall, options)) ?? ({ cancelled: true } as const);
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
    if (!update) {
      return;
    }
    switch (update.sessionUpdate) {
      case 'agent_message_chunk':
        this.events.onTextChunk?.(extractText(update.content), false);
        break;
      case 'agent_thought_chunk':
        this.events.onTextChunk?.(extractText(update.content), true);
        break;
      case 'tool_call':
        this.events.onToolCall?.(toolCallOf(update));
        break;
      case 'tool_call_update':
        this.events.onToolCallUpdate?.(toolCallOf(update));
        break;
      case 'config_option_update':
        this.events.onConfigOptions?.(parseConfigOptions(update.configOptions));
        break;
      case 'current_mode_update':
        this.events.onModeChanged?.(update.currentModeId);
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

  async initialize(): Promise<void> {
    await this.request('initialize', {
      protocolVersion: 1,
      clientCapabilities: {},
      clientInfo: { name: 'claurst-vscode', version: this.clientVersion },
    });
  }

  async newSession(cwd: string): Promise<SessionStart> {
    const result = await this.request<any>('session/new', { cwd, mcpServers: [] });
    this.sessionId = result.sessionId;
    return {
      sessionId: result.sessionId,
      modes: result.modes,
      configOptions: parseConfigOptions(result.configOptions),
    };
  }

  async prompt(text: string): Promise<void> {
    await this.request('session/prompt', {
      sessionId: this.requireSession(),
      prompt: [{ type: 'text', text }],
    });
  }

  /** Change the model, the account, or the reasoning effort. Returns the whole
   * option set, since changing one of them can change the others' values. */
  async setConfigOption(configId: string, value: string): Promise<ConfigOption[]> {
    const result = await this.request<any>('session/set_config_option', {
      sessionId: this.requireSession(),
      configId,
      value,
    });
    return parseConfigOptions(result?.configOptions);
  }

  async setMode(modeId: string): Promise<void> {
    await this.request('session/set_mode', { sessionId: this.requireSession(), modeId });
  }

  cancel(): void {
    if (this.sessionId) {
      this.notify('session/cancel', { sessionId: this.sessionId });
    }
  }

  dispose(): void {
    this.rl.close();
    this.child.kill();
  }

  private requireSession(): string {
    if (!this.sessionId) {
      throw new Error('no active session; call newSession() first');
    }
    return this.sessionId;
  }
}

function toolCallOf(update: any): ToolCallUpdate {
  return {
    toolCallId: update.toolCallId,
    title: update.title,
    status: update.status,
    kind: update.kind,
  };
}

function extractText(content: any): string {
  if (content?.type === 'text') {
    return content.text ?? '';
  }
  return '';
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
