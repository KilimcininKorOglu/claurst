// Webview-side script. Runs in a restricted context with no Node access;
// all agent communication goes through the extension host via postMessage.
//
// Bundled to out/webview.js by esbuild. It shares no module with the extension
// host on purpose: the two run in different sandboxes, and anything that
// reached for `vscode` here would fail at load rather than at the call.
import { renderMarkdown } from './markdown';

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

interface ToolCallLocation {
  path: string;
  line?: number;
}

interface ToolCallMessage {
  type: 'toolCall' | 'toolCallUpdate';
  toolCallId?: string;
  title?: string;
  status?: string;
  kind?: string;
  locations?: ToolCallLocation[];
  output?: string;
  diffs?: Diff[];
  terminalId?: string;
}

interface PermissionOption {
  optionId: string;
  name: string;
  kind: string;
}

interface PermissionMessage {
  type: 'permission';
  requestId: number;
  title: string;
  description?: string;
  diffs?: Diff[];
  locations?: ToolCallLocation[];
  options: PermissionOption[];
}

type HostMessage =
  | { type: 'textChunk'; text: string; kind?: string }
  | { type: 'image'; mimeType: string; data: string; kind?: string }
  | ToolCallMessage
  | PermissionMessage
  | { type: 'terminalOutput'; terminalId: string; chunk: string }
  | { type: 'status'; text: string }
  | { type: 'header'; pills?: Pill[] }
  | { type: 'plan'; entries?: PlanEntry[] }
  | { type: 'commands'; commands?: Command[] }
  | { type: 'capabilities'; image: boolean }
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
  const jumpBtn = document.getElementById('jump-btn') as HTMLButtonElement;
  const attachmentsEl = document.getElementById('attachments') as HTMLElement;

  /** The bubble being streamed into, and everything it has been sent so far.
   *
   * Markdown cannot be appended a chunk at a time: a fence is only a fence
   * once its closing line arrives. The raw text is kept and re-rendered. */
  let currentAgentBubble: HTMLElement | null = null;
  let currentAgentText = '';
  let renderScheduled = false;
  /** Whether the view follows new output. Set by where the reader scrolled. */
  let pinned = true;
  const toolCallEls = new Map<string, HTMLElement>();
  /** What each hosted terminal has said, and where it is being drawn. */
  const terminalText = new Map<string, string>();
  const terminalEls = new Map<string, HTMLElement>();
  let commands: Command[] = [];
  let completionIndex = 0;
  let completionMatches: Command[] = [];
  /** Whether the list is something to choose from, or just the hint for the
   * command already being typed. Arrow keys and Tab belong to the first. */
  let completionMode: 'pick' | 'hint' = 'pick';
  /** Images pasted into the box, waiting for the prompt that carries them. */
  const attachments: Array<{ mimeType: string; data: string }> = [];
  /** Whether the agent said it takes images. Pasting one otherwise would
   * attach something it is going to drop. */
  let acceptsImages = false;

  function appendRow(text: string, cls: string): HTMLElement {
    const row = document.createElement('div');
    row.className = 'row ' + cls;
    const bubble = document.createElement('div');
    bubble.className = 'bubble ' + cls;
    bubble.textContent = text;
    row.appendChild(cls === 'thought' ? foldThought(bubble) : bubble);
    messagesEl.appendChild(row);
    scrollToEnd();
    return bubble;
  }

  /** Put reasoning behind a disclosure rather than in the flow.
   *
   * A long chain of thought pushes the answer it led to off the screen. It
   * stays available, and stays where it happened, but it does not compete with
   * the reply for the reader's attention. */
  function foldThought(bubble: HTMLElement): HTMLElement {
    const details = document.createElement('details');
    details.className = 'thought-fold';
    const summary = document.createElement('summary');
    summary.textContent = 'Thinking';
    details.appendChild(summary);
    details.appendChild(bubble);
    return details;
  }

  /** Draw an image the agent sent or the transcript replayed.
   *
   * The bytes arrive base64-encoded and are shown as a data URL, which is the
   * only image source the panel's CSP allows. */
  function appendImage(mimeType: string, data: string, cls: string): void {
    // The type comes from the other side of the protocol, and it is being
    // pasted into a URL. Anything that is not plainly an image type is drawn
    // as a generic one rather than trusted into the data URL.
    const safeType = /^image\/[a-zA-Z0-9.+-]+$/.test(mimeType) ? mimeType : 'image/png';
    const row = document.createElement('div');
    row.className = 'row ' + cls;
    const image = document.createElement('img');
    image.className = 'attachment';
    image.src = `data:${safeType};base64,${data}`;
    row.appendChild(image);
    messagesEl.appendChild(row);
    scrollToEnd();
  }

  /** Stop streaming into the current bubble.
   *
   * Anything that interrupts the agent's prose (a tool call, an image, a
   * question) closes it, so the next chunk starts a bubble of its own instead
   * of reopening one the reader has already scrolled past. */
  function endBubble(): void {
    if (currentAgentBubble && currentAgentText.length > 0) {
      drawBubble();
    }
    currentAgentBubble = null;
    currentAgentText = '';
  }

  /** Put what has arrived so far into the bubble.
   *
   * The agent's own words are markdown; the user's are shown exactly as typed,
   * because they are the record of what was asked. */
  function drawBubble(): void {
    const bubble = currentAgentBubble;
    if (!bubble) {
      return;
    }
    if (bubble.dataset.cls === 'user') {
      bubble.textContent = currentAgentText;
      return;
    }
    bubble.innerHTML = renderMarkdown(currentAgentText);
    decorateCodeBlocks(bubble);
  }

  /** Put a copy and an apply action on every fenced block.
   *
   * Code an agent writes is code somebody wants somewhere else, and selecting
   * it by hand out of a scrolling transcript loses the indentation as often as
   * not. The buttons are rebuilt with the bubble on each redraw, which is
   * cheap next to the render itself. */
  function decorateCodeBlocks(bubble: HTMLElement): void {
    for (const pre of Array.from(bubble.querySelectorAll('pre'))) {
      const code = pre.querySelector('code');
      if (!code) {
        continue;
      }
      // textContent, not innerText: the highlighter wraps the source in spans,
      // and only textContent gives back exactly what was between the fences.
      const source = code.textContent ?? '';
      const actions = document.createElement('div');
      actions.className = 'code-actions';
      actions.appendChild(
        codeButton('Copy', (button) => {
          navigator.clipboard.writeText(source).then(
            () => flash(button, 'Copied'),
            // A refused clipboard is worth saying out loud: the user would
            // otherwise paste whatever was there before.
            () => flash(button, 'Copy failed'),
          );
        }),
      );
      actions.appendChild(
        codeButton('Apply', () => vscode.postMessage({ type: 'applyCode', text: source })),
      );
      pre.appendChild(actions);
    }
  }

  function codeButton(label: string, onClick: (button: HTMLButtonElement) => void): HTMLButtonElement {
    const button = document.createElement('button');
    button.className = 'code-action';
    button.textContent = label;
    button.addEventListener('click', () => onClick(button));
    return button;
  }

  /** Say what happened on the button that was pressed, then put it back. */
  function flash(button: HTMLButtonElement, message: string): void {
    const original = button.textContent ?? '';
    button.textContent = message;
    setTimeout(() => {
      button.textContent = original;
    }, 1200);
  }

  /** Re-render at most once a frame.
   *
   * A chunk can be a single token, and re-rendering the whole message on each
   * one turns a long answer quadratic. Coalescing keeps the cost proportional
   * to how long the answer takes rather than to how finely it is chopped. */
  function scheduleBubbleDraw(): void {
    if (renderScheduled) {
      return;
    }
    renderScheduled = true;
    requestAnimationFrame(() => {
      renderScheduled = false;
      drawBubble();
      scrollToEnd();
    });
  }

  /** Follow the conversation only while the reader is already at the end.
   *
   * Every append used to pin the view to the bottom, so scrolling up during a
   * turn was undone by the next chunk. A reader who has scrolled away is
   * reading something; the button tells them there is more below. */
  function scrollToEnd(): void {
    if (pinned) {
      messagesEl.scrollTop = messagesEl.scrollHeight;
    } else {
      jumpBtn.classList.remove('hidden');
    }
  }

  /** Within a line's height of the bottom counts as being at the bottom: an
   * exact comparison never holds once a fractional scroll position appears. */
  function atEnd(): boolean {
    return messagesEl.scrollHeight - messagesEl.scrollTop - messagesEl.clientHeight < 24;
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

  /** The files a call is about, each one a way to open it.
   *
   * Only the last path segment is shown: the agent sends absolute paths, and a
   * full one would push the tool's own title off the row. The whole path is on
   * the tooltip. */
  function renderLocations(locations: ToolCallLocation[]): HTMLElement {
    const row = document.createElement('div');
    row.className = 'tool-locations';
    for (const location of locations) {
      const link = document.createElement('button');
      link.className = 'tool-location';
      const name = location.path.split(/[\\/]/).pop() || location.path;
      link.textContent = location.line === undefined ? name : `${name}:${location.line}`;
      link.title = location.path;
      link.addEventListener('click', () =>
        vscode.postMessage({ type: 'openLocation', path: location.path, line: location.line }),
      );
      row.appendChild(link);
    }
    return row;
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

    if (msg.locations && msg.locations.length > 0) {
      el.appendChild(renderLocations(msg.locations));
    }

    for (const diff of msg.diffs || []) {
      el.appendChild(renderDiff(diff));
    }
    // What the tool returned. Long output is capped by the element rather than
    // by cutting the text, so scrolling still reaches the end of it.
    if (msg.output) {
      const output = document.createElement('div');
      output.className = 'tool-output';
      output.textContent = msg.output;
      el.appendChild(output);
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
    scrollToEnd();
  }

  /** Ask the question where the answer will be remembered.
   *
   * The block stays in the transcript after it is answered, showing which
   * option was chosen, so scrolling back says what was approved and when. */
  function renderPermission(msg: PermissionMessage): void {
    const block = document.createElement('div');
    block.className = 'permission';

    const heading = document.createElement('div');
    heading.className = 'permission-title';
    heading.textContent = msg.title;
    block.appendChild(heading);

    if (msg.locations && msg.locations.length > 0) {
      block.appendChild(renderLocations(msg.locations));
    }

    if (msg.description) {
      const body = document.createElement('div');
      body.className = 'permission-body';
      body.textContent = msg.description;
      block.appendChild(body);
    }

    for (const diff of msg.diffs || []) {
      block.appendChild(renderDiff(diff));
    }

    const row = document.createElement('div');
    row.className = 'permission-options';
    for (const option of msg.options) {
      const button = document.createElement('button');
      button.className = 'permission-option ' + option.kind;
      button.textContent = option.name;
      button.addEventListener('click', () => {
        vscode.postMessage({
          type: 'permissionAnswer',
          requestId: msg.requestId,
          optionId: option.optionId,
        });
        const chosen = document.createElement('div');
        chosen.className = 'permission-chosen';
        chosen.textContent = option.name;
        row.replaceWith(chosen);
      });
      row.appendChild(button);
    }
    block.appendChild(row);

    messagesEl.appendChild(block);
    scrollToEnd();
  }

  function appendTerminalOutput(terminalId: string, chunk: string): void {
    const text = (terminalText.get(terminalId) || '') + chunk;
    terminalText.set(terminalId, text);
    const pre = terminalEls.get(terminalId);
    if (pre) {
      pre.textContent = text;
      scrollToEnd();
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
      const selected = completionMode === 'pick' && index === completionIndex;
      row.className = 'completion' + (selected ? ' selected' : '');
      const name = document.createElement('span');
      name.className = 'completion-name';
      name.textContent = '/' + command.name;
      // What the command takes, which is the only thing worth reading once the
      // name has been settled.
      if (command.hint) {
        const hint = document.createElement('span');
        hint.className = 'completion-hint';
        hint.textContent = command.hint;
        name.appendChild(document.createTextNode(' '));
        name.appendChild(hint);
      }
      const description = document.createElement('span');
      description.className = 'completion-description';
      description.textContent = command.description || '';
      row.appendChild(name);
      row.appendChild(description);
      if (completionMode === 'pick') {
        row.addEventListener('mousedown', (e) => {
          e.preventDefault();
          applyCompletion(command);
        });
      }
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
    completionIndex = 0;

    const naming = /^\/([a-zA-Z0-9-]*)$/.exec(text);
    if (naming) {
      const typed = naming[1].toLowerCase();
      completionMode = 'pick';
      completionMatches = commands
        .filter((command) => command.name.toLowerCase().startsWith(typed))
        .slice(0, 8);
      renderCompletions();
      return;
    }

    // Once a space has been typed the command is settled and the list is no
    // longer something to choose from. What is still worth showing is what
    // that command takes, which used to disappear the moment it was needed.
    const running = /^\/([a-zA-Z0-9-]+)\s/.exec(text);
    const named = running
      ? commands.find((command) => command.name.toLowerCase() === running[1].toLowerCase())
      : undefined;
    completionMode = 'hint';
    completionMatches = named ? [named] : [];
    renderCompletions();
  }

  /** Put the chosen path where the `@` was typed.
   *
   * Appending it to the end put the file at the end of the sentence however
   * far back the mention had been started, so a question typed around a
   * mention came out reordered. */
  function replaceMention(target: string): void {
    const caret = inputEl.selectionStart ?? inputEl.value.length;
    const partial = /(^|\s)@([^\s]*)$/.exec(inputEl.value.slice(0, caret));
    const start = partial ? caret - partial[2].length : caret;
    const head = inputEl.value.slice(0, start) + target + ' ';
    inputEl.value = head + inputEl.value.slice(caret);
    inputEl.setSelectionRange(head.length, head.length);
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
    // A freshly typed '@' is a request to pick a file; the extension host is
    // the only side that can read the workspace. It is measured against the
    // caret, not the end of the box, so editing an earlier sentence works.
    const caret = inputEl.selectionStart ?? inputEl.value.length;
    if (/(^|\s)@$/.test(inputEl.value.slice(0, caret))) {
      vscode.postMessage({ type: 'pickFile' });
    }
  });

  /** Take an image off the clipboard and hold it until the prompt is sent.
   *
   * The agent declares it accepts images, so a screenshot of a failing test or
   * a mock-up can go straight into the question instead of being described. */
  function handlePaste(event: ClipboardEvent): void {
    if (!acceptsImages) {
      return;
    }
    const items = Array.from(event.clipboardData?.items ?? []).filter((item) =>
      item.type.startsWith('image/'),
    );
    if (items.length === 0) {
      return;
    }
    event.preventDefault();
    for (const item of items) {
      const file = item.getAsFile();
      if (file) {
        readAttachment(file);
      }
    }
  }

  function readAttachment(file: File): void {
    const reader = new FileReader();
    reader.onload = () => {
      const url = String(reader.result);
      const comma = url.indexOf(',');
      if (comma < 0) {
        return;
      }
      attachments.push({ mimeType: file.type, data: url.slice(comma + 1) });
      renderAttachments();
    };
    // A file that cannot be read must not be silently dropped: the user would
    // send a question about a screenshot that never went with it.
    reader.onerror = () => appendRow(`Could not read the pasted image.`, 'system');
    reader.readAsDataURL(file);
  }

  /** Show what is going to travel with the next prompt, and let it be removed. */
  function renderAttachments(): void {
    attachmentsEl.textContent = '';
    attachmentsEl.classList.toggle('hidden', attachments.length === 0);
    attachments.forEach((attachment, index) => {
      const wrapper = document.createElement('div');
      wrapper.className = 'attachment-chip';
      const thumb = document.createElement('img');
      thumb.src = `data:${attachment.mimeType};base64,${attachment.data}`;
      wrapper.appendChild(thumb);
      const remove = document.createElement('button');
      remove.className = 'attachment-remove';
      remove.textContent = '✕';
      remove.title = 'Remove this image';
      remove.addEventListener('click', () => {
        attachments.splice(index, 1);
        renderAttachments();
      });
      wrapper.appendChild(remove);
      attachmentsEl.appendChild(wrapper);
    });
  }

  function send(): void {
    const text = inputEl.value.trim();
    if (!text && attachments.length === 0) {
      return;
    }
    // Asking a question is a request to watch the answer, wherever the reader
    // had scrolled to before.
    pinned = true;
    jumpBtn.classList.add('hidden');
    if (text) {
      appendRow(text, 'user');
    }
    for (const attachment of attachments) {
      appendImage(attachment.mimeType, attachment.data, 'user');
    }
    const images = attachments.splice(0, attachments.length);
    renderAttachments();
    inputEl.value = '';
    completionMatches = [];
    renderCompletions();
    autoResize();
    endBubble();
    setBusy(true);
    vscode.postMessage({ type: 'prompt', text, images });
  }

  // Where the reader is decides whether new output moves the view. A scroll
  // back to the bottom re-arms following without a click.
  messagesEl.addEventListener('scroll', () => {
    pinned = atEnd();
    jumpBtn.classList.toggle('hidden', pinned);
  });

  jumpBtn.addEventListener('click', () => {
    pinned = true;
    jumpBtn.classList.add('hidden');
    messagesEl.scrollTop = messagesEl.scrollHeight;
  });

  inputEl.addEventListener('paste', handlePaste);
  sendBtn.addEventListener('click', send);
  stopBtn.addEventListener('click', () => vscode.postMessage({ type: 'stop' }));
  inputEl.addEventListener('keydown', (e) => {
    // In 'hint' mode the list is a label, not a menu: Enter must send the
    // command the user finished typing rather than replace it with itself.
    if (completionMode === 'pick' && completionMatches.length > 0) {
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
          endBubble();
          currentAgentBubble = appendRow('', cls);
          currentAgentBubble.dataset.cls = cls;
        }
        currentAgentText += msg.text;
        scheduleBubbleDraw();
        break;
      }
      case 'toolCall':
      case 'toolCallUpdate': {
        endBubble();
        upsertToolCall(msg);
        break;
      }
      case 'image': {
        endBubble();
        appendImage(msg.mimeType, msg.data, msg.kind || 'agent');
        break;
      }
      case 'permission': {
        endBubble();
        renderPermission(msg);
        break;
      }
      case 'terminalOutput': {
        appendTerminalOutput(msg.terminalId, msg.chunk);
        break;
      }
      case 'status': {
        endBubble();
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
      case 'capabilities': {
        acceptsImages = msg.image;
        inputEl.placeholder = acceptsImages
          ? 'Ask claurst... (/ for commands, @ for files, paste an image)'
          : 'Ask claurst... (/ for commands, @ for files)';
        break;
      }
      case 'mention': {
        replaceMention(msg.text);
        autoResize();
        inputEl.focus();
        break;
      }
      case 'turnEnded': {
        endBubble();
        setBusy(false);
        break;
      }
      default:
        break;
    }
  });
})();
