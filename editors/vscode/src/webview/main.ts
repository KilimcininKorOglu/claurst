// Webview-side script. Runs in a restricted context with no Node access;
// all agent communication goes through the extension host via postMessage.
//
// Bundled to out/webview.js by esbuild. It shares no module with the extension
// host on purpose: the two run in different sandboxes, and anything that
// reached for `vscode` here would fail at load rather than at the call.

interface VsCodeApi {
  postMessage(message: unknown): void;
  getState(): unknown;
  setState(state: unknown): void;
}

declare function acquireVsCodeApi(): VsCodeApi;

interface Pill {
  key: string;
  label: string;
  value: string;
}

interface PlanEntry {
  content: string;
  status?: string;
}

interface Command {
  name: string;
  description?: string;
  hint?: string;
}

interface Diff {
  path: string;
  oldText?: string | null;
  newText?: string;
}

interface ToolCallMessage {
  type: 'toolCall' | 'toolCallUpdate';
  toolCallId?: string;
  title?: string;
  status?: string;
  kind?: string;
  diffs?: Diff[];
  terminalId?: string;
}

type HostMessage =
  | { type: 'textChunk'; text: string; kind?: string }
  | ToolCallMessage
  | { type: 'terminalOutput'; terminalId: string; chunk: string }
  | { type: 'status'; text: string }
  | { type: 'header'; pills?: Pill[] }
  | { type: 'plan'; entries?: PlanEntry[] }
  | { type: 'commands'; commands?: Command[] }
  | { type: 'mention'; text: string }
  | { type: 'turnEnded' };

(function () {
  const vscode = acquireVsCodeApi();
  const headerEl = document.getElementById('header') as HTMLElement;
  const planEl = document.getElementById('plan') as HTMLElement;
  const messagesEl = document.getElementById('messages') as HTMLElement;
  const completionsEl = document.getElementById('completions') as HTMLElement;
  const inputEl = document.getElementById('input-box') as HTMLTextAreaElement;
  const sendBtn = document.getElementById('send-btn') as HTMLButtonElement;
  const stopBtn = document.getElementById('stop-btn') as HTMLButtonElement;

  let currentAgentBubble: HTMLElement | null = null;
  const toolCallEls = new Map<string, HTMLElement>();
  /** What each hosted terminal has said, and where it is being drawn. */
  const terminalText = new Map<string, string>();
  const terminalEls = new Map<string, HTMLElement>();
  let commands: Command[] = [];
  let completionIndex = 0;
  let completionMatches: Command[] = [];

  function appendRow(text: string, cls: string): HTMLElement {
    const row = document.createElement('div');
    row.className = 'row ' + cls;
    const bubble = document.createElement('div');
    bubble.className = 'bubble ' + cls;
    bubble.textContent = text;
    row.appendChild(bubble);
    messagesEl.appendChild(row);
    messagesEl.scrollTop = messagesEl.scrollHeight;
    return bubble;
  }

  function statusIcon(status: string | undefined): string {
    if (status === 'completed') return '✓';
    if (status === 'failed') return '✗';
    if (status === 'in_progress' || status === 'pending') return '◌';
    return '•';
  }

  // A diff is drawn line by line rather than as a blob of text: which lines
  // moved is the whole point of showing it.
  function renderDiff(diff: Diff): HTMLElement {
    const wrapper = document.createElement('div');
    wrapper.className = 'diff';

    const title = document.createElement('div');
    title.className = 'diff-path';
    title.textContent = diff.path;
    wrapper.appendChild(title);

    const before = (diff.oldText || '').split('\n');
    const after = (diff.newText || '').split('\n');
    const shared = commonPrefix(before, after);
    const tailLength = commonSuffix(before, after, shared);

    appendDiffLines(wrapper, before.slice(0, shared).slice(-3), 'context');
    appendDiffLines(wrapper, before.slice(shared, before.length - tailLength), 'removed');
    appendDiffLines(wrapper, after.slice(shared, after.length - tailLength), 'added');
    appendDiffLines(wrapper, after.slice(after.length - tailLength).slice(0, 3), 'context');
    return wrapper;
  }

  function commonPrefix(before: string[], after: string[]): number {
    let i = 0;
    while (i < before.length && i < after.length && before[i] === after[i]) {
      i += 1;
    }
    return i;
  }

  function commonSuffix(before: string[], after: string[], prefix: number): number {
    let i = 0;
    while (
      i < before.length - prefix &&
      i < after.length - prefix &&
      before[before.length - 1 - i] === after[after.length - 1 - i]
    ) {
      i += 1;
    }
    return i;
  }

  function appendDiffLines(wrapper: HTMLElement, lines: string[], cls: string): void {
    const marker = cls === 'added' ? '+' : cls === 'removed' ? '-' : ' ';
    for (const line of lines) {
      const el = document.createElement('div');
      el.className = 'diff-line ' + cls;
      el.textContent = marker + line;
      wrapper.appendChild(el);
    }
  }

  function upsertToolCall(msg: ToolCallMessage): void {
    let el = msg.toolCallId ? toolCallEls.get(msg.toolCallId) : undefined;
    if (!el) {
      el = document.createElement('div');
      el.className = 'tool-call';
      messagesEl.appendChild(el);
      if (msg.toolCallId) {
        toolCallEls.set(msg.toolCallId, el);
      }
    }
    el.className = 'tool-call ' + (msg.status || '');
    el.textContent = '';

    const heading = document.createElement('div');
    heading.className = 'tool-title';
    heading.textContent = `${statusIcon(msg.status)} ${msg.title || '(tool call)'}`;
    el.appendChild(heading);

    for (const diff of msg.diffs || []) {
      el.appendChild(renderDiff(diff));
    }
    // A tool call is redrawn from scratch on every update, so the terminal's
    // element is rebuilt too and refilled from what it has said so far.
    if (msg.terminalId) {
      const pre = document.createElement('pre');
      pre.className = 'terminal-output';
      pre.textContent = terminalText.get(msg.terminalId) || '';
      terminalEls.set(msg.terminalId, pre);
      el.appendChild(pre);
    }
    messagesEl.scrollTop = messagesEl.scrollHeight;
  }

  function appendTerminalOutput(terminalId: string, chunk: string): void {
    const text = (terminalText.get(terminalId) || '') + chunk;
    terminalText.set(terminalId, text);
    const pre = terminalEls.get(terminalId);
    if (pre) {
      pre.textContent = text;
      messagesEl.scrollTop = messagesEl.scrollHeight;
    }
  }

  // The pills are whatever the agent said it offers, so they are rebuilt from
  // the message rather than hardcoded here.
  function renderHeader(pills: Pill[]): void {
    headerEl.textContent = '';
    for (const pill of pills) {
      const button = document.createElement('button');
      button.className = 'pill';
      button.textContent = `${pill.label}: ${pill.value}`;
      button.title = `Change ${pill.label}`;
      button.addEventListener('click', () => vscode.postMessage({ type: 'pick', key: pill.key }));
      headerEl.appendChild(button);
    }
  }

  function planIcon(status: string | undefined): string {
    if (status === 'completed') return '✓';
    if (status === 'in_progress') return '▸';
    return '○';
  }

  function renderPlan(entries: PlanEntry[]): void {
    planEl.textContent = '';
    planEl.classList.toggle('hidden', entries.length === 0);
    for (const entry of entries) {
      const row = document.createElement('div');
      row.className = 'plan-entry ' + (entry.status || '');
      row.textContent = `${planIcon(entry.status)} ${entry.content}`;
      planEl.appendChild(row);
    }
  }

  function renderCompletions(): void {
    completionsEl.textContent = '';
    completionsEl.classList.toggle('hidden', completionMatches.length === 0);
    completionMatches.forEach((command, index) => {
      const row = document.createElement('div');
      row.className = 'completion' + (index === completionIndex ? ' selected' : '');
      const name = document.createElement('span');
      name.className = 'completion-name';
      name.textContent = '/' + command.name;
      const description = document.createElement('span');
      description.className = 'completion-description';
      description.textContent = command.description || '';
      row.appendChild(name);
      row.appendChild(description);
      row.addEventListener('mousedown', (e) => {
        e.preventDefault();
        applyCompletion(command);
      });
      completionsEl.appendChild(row);
    });
  }

  function applyCompletion(command: Command): void {
    inputEl.value = '/' + command.name + ' ';
    completionMatches = [];
    renderCompletions();
    inputEl.focus();
  }

  function updateCompletions(): void {
    const text = inputEl.value;
    const match = /^\/([a-zA-Z0-9-]*)$/.exec(text);
    if (!match) {
      completionMatches = [];
      renderCompletions();
      return;
    }
    const typed = match[1].toLowerCase();
    completionMatches = commands
      .filter((command) => command.name.toLowerCase().startsWith(typed))
      .slice(0, 8);
    completionIndex = 0;
    renderCompletions();
  }

  function setBusy(busy: boolean): void {
    sendBtn.disabled = busy;
    stopBtn.classList.toggle('hidden', !busy);
  }

  function autoResize(): void {
    inputEl.style.height = 'auto';
    inputEl.style.height = Math.min(inputEl.scrollHeight, 200) + 'px';
  }

  inputEl.addEventListener('input', () => {
    autoResize();
    updateCompletions();
    // A bare '@' is a request to pick a file; the extension host is the only
    // side that can read the workspace.
    if (inputEl.value.endsWith('@')) {
      vscode.postMessage({ type: 'pickFile' });
    }
  });

  function send(): void {
    const text = inputEl.value.trim();
    if (!text) {
      return;
    }
    appendRow(text, 'user');
    inputEl.value = '';
    completionMatches = [];
    renderCompletions();
    autoResize();
    currentAgentBubble = null;
    setBusy(true);
    vscode.postMessage({ type: 'prompt', text });
  }

  sendBtn.addEventListener('click', send);
  stopBtn.addEventListener('click', () => vscode.postMessage({ type: 'stop' }));
  inputEl.addEventListener('keydown', (e) => {
    if (completionMatches.length > 0) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        completionIndex = (completionIndex + 1) % completionMatches.length;
        renderCompletions();
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        completionIndex =
          (completionIndex - 1 + completionMatches.length) % completionMatches.length;
        renderCompletions();
        return;
      }
      if (e.key === 'Tab' || (e.key === 'Enter' && !e.shiftKey)) {
        e.preventDefault();
        applyCompletion(completionMatches[completionIndex]);
        return;
      }
      if (e.key === 'Escape') {
        completionMatches = [];
        renderCompletions();
        return;
      }
    }
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  });

  setBusy(false);

  window.addEventListener('message', (event: MessageEvent<HostMessage>) => {
    const msg = event.data;
    switch (msg.type) {
      case 'textChunk': {
        const cls = msg.kind || 'agent';
        if (!currentAgentBubble || currentAgentBubble.dataset.cls !== cls) {
          currentAgentBubble = appendRow('', cls);
          currentAgentBubble.dataset.cls = cls;
        }
        currentAgentBubble.textContent += msg.text;
        messagesEl.scrollTop = messagesEl.scrollHeight;
        break;
      }
      case 'toolCall':
      case 'toolCallUpdate': {
        currentAgentBubble = null;
        upsertToolCall(msg);
        break;
      }
      case 'terminalOutput': {
        appendTerminalOutput(msg.terminalId, msg.chunk);
        break;
      }
      case 'status': {
        currentAgentBubble = null;
        appendRow(msg.text, 'system');
        break;
      }
      case 'header': {
        renderHeader(msg.pills || []);
        break;
      }
      case 'plan': {
        renderPlan(msg.entries || []);
        break;
      }
      case 'commands': {
        commands = msg.commands || [];
        break;
      }
      case 'mention': {
        inputEl.value += msg.text + ' ';
        autoResize();
        inputEl.focus();
        break;
      }
      case 'turnEnded': {
        currentAgentBubble = null;
        setBusy(false);
        break;
      }
      default:
        break;
    }
  });
})();
