// The parts of the protocol that are only shape: reading what the agent sent
// into what the panel draws, and back again.
//
// Split out of `acpClient` so it can be tested. That module opens a child
// process and imports `vscode`, neither of which exists under `node --test`,
// and neither of which any of this needs.

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

/** What the agent said it can do, read from its `initialize` answer.
 *
 * Everything defaults to false: an agent that did not claim a capability is
 * one this must not use, and guessing would send it something it will drop. */
export type AgentCapabilities = {
  /** Its name and version, for the panel to show. */
  name?: string;
  version?: string;
  /** Whether a prompt may carry an image block. */
  image: boolean;
  /** Whether a prompt may carry a file's contents inline. */
  embeddedContext: boolean;
  /** Whether a stored session can be reopened at all. */
  loadSession: boolean;
};

/** Read the agent's answer to `initialize`.
 *
 * A capability it did not claim reads as false rather than as unknown: sending
 * a block the agent cannot carry loses whatever the user attached, and there
 * is nothing in the answer to distinguish "no" from "old agent". */
export function capabilitiesOf(result: any): AgentCapabilities {
  const prompt = result?.agentCapabilities?.promptCapabilities;
  return {
    name: typeof result?.agentInfo?.name === 'string' ? result.agentInfo.name : undefined,
    version: typeof result?.agentInfo?.version === 'string' ? result.agentInfo.version : undefined,
    image: prompt?.image === true,
    embeddedContext: prompt?.embeddedContext === true,
    loadSession: result?.agentCapabilities?.loadSession === true,
  };
}

export function startOf(result: any, sessionId: string): SessionStart {
  return {
    sessionId,
    modes: result?.modes,
    configOptions: parseConfigOptions(result?.configOptions),
  };
}

export function toolCallOf(update: any): ToolCallUpdate {
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
export function locationsOf(raw: any): ToolCallLocation[] {
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
export function deliver(handler: SessionEvents, content: any, kind: ChunkKind): void {
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

export function extractText(content: any): string {
  if (content?.type === 'text') {
    return content.text ?? '';
  }
  return '';
}

export function parseCommands(raw: any): AvailableCommand[] {
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

export function parsePlan(raw: any): PlanEntry[] {
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
export function parseConfigOptions(raw: any): ConfigOption[] {
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

/** Every `@path` mention in what the user typed.
 *
 * A mention runs to the next space, which is why anything the panel appends
 * after a path (a line range, a note) has to be separated from it. */
export function mentionsIn(text: string): string[] {
  const found = new Set<string>();
  const pattern = /(^|\s)@([^\s]+)/g;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(text)) !== null) {
    found.add(match[2]);
  }
  return [...found];
}
