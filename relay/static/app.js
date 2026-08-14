// Web client for the relay.
//
// The token lives in an HttpOnly cookie set by POST /api/client/auth, so this
// script never holds it and cannot read it back. Authentication state is
// therefore inferred from whether an API call succeeds, not from stored state.
//
// Every value that arrives from the relay is written with textContent. It came
// from a machine the operator controls, but it is still transcript data and
// must not be parsed as markup.

'use strict';

const views = {
  token: document.getElementById('view-token'),
  workspace: document.getElementById('workspace'),
};

const el = {
  tokenForm: document.getElementById('token-form'),
  tokenInput: document.getElementById('token-input'),
  tokenError: document.getElementById('token-error'),
  refresh: document.getElementById('refresh'),
  sessionList: document.getElementById('session-list'),
  sessionsEmpty: document.getElementById('sessions-empty'),
  back: document.getElementById('back'),
  cancel: document.getElementById('cancel'),
  sessionTitle: document.getElementById('session-title'),
  stream: document.getElementById('stream'),
  permission: document.getElementById('permission'),
  permissionTool: document.getElementById('permission-tool'),
  permissionDesc: document.getElementById('permission-desc'),
  permissionActions: document.getElementById('permission-actions'),
  permissionLocal: document.getElementById('permission-local'),
  mcp: document.getElementById('mcp'),
  mcpTitle: document.getElementById('mcp-title'),
  mcpDetail: document.getElementById('mcp-detail'),
  question: document.getElementById('question'),
  questionText: document.getElementById('question-text'),
  questionOptions: document.getElementById('question-options'),
  questionForm: document.getElementById('question-form'),
  questionInput: document.getElementById('question-input'),
  promptForm: document.getElementById('prompt-form'),
  promptInput: document.getElementById('prompt-input'),
  send: document.getElementById('send'),
  status: document.getElementById('session-status'),
  workspace: document.getElementById('workspace'),
  busy: document.getElementById('session-busy'),
  attach: document.getElementById('attach'),
  fileInput: document.getElementById('file-input'),
  attachments: document.getElementById('attachments'),
};

/** Live session view state. Reset whenever a session is opened or left. */
const live = {
  sessionId: null,
  source: null,
  lastSeq: 0,
  bubbles: new Map(),  // message_id -> assistant bubble element
  tools: new Map(),    // tool_id -> tool row element
  permission: null,    // the request currently shown on the card
  mcp: null,           // the MCP trust prompt currently shown on the card
  question: null,      // the AskUserQuestion currently shown on the card
  attachments: [],     // staged files, sent with the next prompt
  retryDelay: 0,       // grows while the stream cannot be reached
};

/**
 * Switch between the token screen and the workspace.
 *
 * `sessions` and `session` are panes of the same workspace, not separate
 * screens: on a wide window both are visible at once, so the pane name only
 * decides which one a narrow window shows.
 */
function show(name) {
  views.token.hidden = name !== 'token';
  views.workspace.hidden = name === 'token';
  if (name === 'sessions' || name === 'session') {
    el.workspace.dataset.pane = name;
  }
}

/**
 * Call the relay, sending the session cookie.
 *
 * A 401 means the cookie is missing or stale, which drops the user back to the
 * token screen rather than leaving a screen that silently stops updating.
 */
async function api(path, options = {}) {
  const response = await fetch(path, {
    credentials: 'same-origin',
    ...options,
  });
  if (response.status === 401) {
    leaveSession();
    show('token');
    throw new Error('unauthorised');
  }
  if (!response.ok) {
    throw new Error(`${options.method || 'GET'} ${path} failed: ${response.status}`);
  }
  return response;
}

// ---------------------------------------------------------------------------
// Token
// ---------------------------------------------------------------------------

el.tokenForm.addEventListener('submit', async (event) => {
  event.preventDefault();
  const token = el.tokenInput.value.trim();
  el.tokenError.hidden = true;

  if (token.length < 32) {
    el.tokenError.textContent = 'The relay token is at least 32 characters.';
    el.tokenError.hidden = false;
    return;
  }

  let response;
  try {
    response = await fetch('/api/client/auth', {
      method: 'POST',
      credentials: 'same-origin',
      headers: { Authorization: `Bearer ${token}` },
    });
  } catch {
    el.tokenError.textContent = 'Could not reach the relay.';
    el.tokenError.hidden = false;
    return;
  }

  if (!response.ok) {
    el.tokenError.textContent = 'That token was rejected.';
    el.tokenError.hidden = false;
    return;
  }

  el.tokenInput.value = '';
  await openSessions();
});

// ---------------------------------------------------------------------------
// Session list
// ---------------------------------------------------------------------------

function describeIdle(seconds) {
  if (seconds < 60) return `active ${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  return `active ${minutes}m ago`;
}

/**
 * Refresh the sidebar list.
 *
 * Deliberately does not touch the open session: the sidebar is permanent on a
 * wide window, so refreshing it must not tear down the transcript next to it.
 */
async function loadSessions() {
  const sessions = await (await api('/api/client/sessions')).json();

  el.sessionList.replaceChildren();
  el.sessionsEmpty.hidden = sessions.length > 0;

  for (const session of sessions) {
    const label = document.createElement('span');
    label.className = 'label';
    label.textContent = session.label || session.session_id;

    const meta = document.createElement('span');
    meta.className = 'meta';
    meta.textContent = [session.cwd, describeIdle(session.idle_secs)]
      .filter(Boolean)
      .join(' · ');

    const button = document.createElement('button');
    button.type = 'button';
    button.append(label, meta);
    if (session.session_id === live.sessionId) {
      button.setAttribute('aria-current', 'true');
    }
    button.addEventListener('click', () => openSession(session));

    const item = document.createElement('li');
    item.dataset.sessionId = session.session_id;
    item.append(button);
    el.sessionList.append(item);
  }
}

/** Show the list and, on a narrow window, bring it to the front. */
async function openSessions() {
  await loadSessions();
  show('sessions');
}

el.refresh.addEventListener('click', () => {
  loadSessions().catch(reportFailure);
});

// ---------------------------------------------------------------------------
// Session screen
// ---------------------------------------------------------------------------

function leaveSession() {
  if (live.source) {
    live.source.close();
  }
  live.source = null;
  live.sessionId = null;
  live.lastSeq = 0;
  live.bubbles.clear();
  live.tools.clear();
  live.permission = null;
  live.mcp = null;
  live.question = null;
  live.attachments = [];
  live.retryDelay = 0;
  el.permission.hidden = true;
  el.mcp.hidden = true;
  el.question.hidden = true;
  el.stream.replaceChildren();
  el.status.hidden = true;
  el.busy.hidden = true;
  renderAttachments();
}

// Narrow windows only: bring the list forward without dropping the session, so
// returning to it is instant and the transcript is not refetched.
el.back.addEventListener('click', () => {
  show('sessions');
  loadSessions().catch(reportFailure);
});

function openSession(session) {
  if (session.session_id === live.sessionId) {
    // Already attached; just bring the pane forward on a narrow window.
    show('session');
    return;
  }
  leaveSession();
  live.sessionId = session.session_id;
  el.sessionTitle.textContent = session.label || session.session_id;
  show('session');
  markCurrentSession();
  connectStream();
}

/** Mark the open session in the sidebar, which stays visible beside it. */
function markCurrentSession() {
  for (const button of el.sessionList.querySelectorAll('button')) {
    button.removeAttribute('aria-current');
  }
  const index = [...el.sessionList.children].findIndex(
    (item) => item.dataset.sessionId === live.sessionId,
  );
  if (index >= 0) {
    el.sessionList.children[index].firstElementChild.setAttribute('aria-current', 'true');
  }
}

/**
 * Attach to the session's event stream.
 *
 * `since` carries the last sequence number seen, so a reconnect resumes from
 * the ring buffer instead of replaying or losing the transcript.
 */
function connectStream() {
  const url = `/api/client/sessions/${encodeURIComponent(live.sessionId)}/stream?since=${live.lastSeq}`;
  const source = new EventSource(url, { withCredentials: true });
  live.source = source;

  source.addEventListener('message', (event) => {
    if (event.lastEventId) {
      const seq = Number.parseInt(event.lastEventId, 10);
      if (Number.isFinite(seq)) {
        live.lastSeq = seq;
      }
    }
    let payload;
    try {
      payload = JSON.parse(event.data);
    } catch {
      return;
    }
    render(payload);
  });

  source.addEventListener('error', () => {
    // EventSource reconnects on its own, but it reuses the original URL and so
    // would replay from the sequence number this stream started at. Close it
    // and reconnect with the current one.
    source.close();
    if (live.source !== source) {
      return;
    }
    // Back off rather than hammering a relay that is down; a fixed retry
    // spins forever at full rate when the machine is simply off.
    live.retryDelay = Math.min((live.retryDelay || 1000) * 2, 30000);
    setStatus(`Reconnecting in ${Math.round(live.retryDelay / 1000)}s…`);
    setTimeout(() => {
      if (live.sessionId) {
        connectStream();
      }
    }, live.retryDelay);
  });

  source.addEventListener('open', () => {
    live.retryDelay = 0;
    el.status.hidden = true;
  });
}

/**
 * Reflect whether a turn is running.
 *
 * The send button stays usable: queuing the next prompt while the model works
 * is normal, and disabling it would be a worse lie than the spinner.
 */
function setBusy(busy) {
  el.busy.hidden = !busy;
  if (!busy) {
    el.status.hidden = true;
  }
}

function setStatus(text) {
  el.status.textContent = text;
  el.status.hidden = false;
}

function reportFailure(error) {
  if (error && error.message === 'unauthorised') {
    return;
  }
  setStatus(error instanceof Error ? error.message : String(error));
}

function atBottom() {
  return el.stream.scrollHeight - el.stream.scrollTop - el.stream.clientHeight < 60;
}

function append(node) {
  const stick = atBottom();
  el.stream.append(node);
  if (stick) {
    el.stream.scrollTop = el.stream.scrollHeight;
  }
}

/// Longest tool output rendered. A build log can run to megabytes, and the
/// transcript is not the place to read one.
const TOOL_OUTPUT_LIMIT = 4000;

function truncate(text, limit) {
  if (text.length <= limit) {
    return text;
  }
  const dropped = text.length - limit;
  return `${text.slice(0, limit)}\n… ${dropped} more character(s) not shown`;
}

function bubble(kind, text) {
  const node = document.createElement('div');
  node.className = `msg ${kind}`;
  node.textContent = text;
  return node;
}

function notice(text, bad) {
  const node = document.createElement('p');
  node.className = bad ? 'notice bad' : 'notice';
  node.textContent = text;
  return node;
}

/** Format a cost with enough precision that a cheap turn is not shown as $0.00. */
function money(value) {
  return `$${value.toFixed(4)}`;
}

/**
 * Bind a figure to its label with a non-breaking space.
 *
 * The usage line wraps on a phone. Without this it breaks between the two and
 * leaves a bare "total" alone on the second row.
 */
function labelled(figure, label) {
  // U+00A0 written as an escape: an invisible byte here would defeat a
  // later grep for this line.
  return `${figure}\u00a0${label}`;
}

/**
 * Explain why a turn ended, when the reason is worth saying out loud.
 *
 * Returns null for the reasons that mean "nothing went wrong": a normal
 * finish, a slash-command reply, and a turn that ended only to run tools.
 * That last one repeats on every tool round, so announcing it would bury the
 * cases that matter.
 *
 * An unrecognised reason is shown verbatim rather than assumed benign; it
 * comes from `StopReason::Other` and could be anything the provider invented.
 */
function stopNotice(stopReason) {
  switch (stopReason) {
    case 'end_turn':
    case 'command':
    case 'tool_use':
      return null;
    case 'max_tokens':
      return notice('The reply hit the output limit and was cut short.', true);
    case 'content_filtered':
      return notice('The provider filtered this response.', true);
    case 'stop_sequence':
      return notice('The model stopped at a configured stop sequence.');
    default:
      return notice(`Turn ended: ${stopReason}`);
  }
}

/**
 * Build the token and cost line shown under a finished turn.
 *
 * Returns null when the turn spent nothing, so a slash-command reply does not
 * get a row of zeroes claiming it cost something to run.
 */
function usageLine(usage) {
  if (!usage) {
    return null;
  }
  const parts = [
    labelled((usage.input_tokens || 0).toLocaleString(), 'in'),
    labelled((usage.output_tokens || 0).toLocaleString(), 'out'),
  ];
  // Creation and read are priced differently but read as one number here; the
  // wire keeps them apart so a future view can split them.
  const cached = (usage.cache_creation_tokens || 0) + (usage.cache_read_tokens || 0);
  if (cached > 0) {
    parts.push(labelled(cached.toLocaleString(), 'cached'));
  }
  if (typeof usage.cost_usd === 'number') {
    parts.push(labelled(money(usage.cost_usd), 'turn'));
  }
  if (typeof usage.session_cost_usd === 'number') {
    parts.push(labelled(money(usage.session_cost_usd), 'total'));
  }

  const node = document.createElement('p');
  node.className = 'usage';
  node.textContent = parts.join(' · ');
  return node;
}

function render(event) {
  switch (event.type) {
    case 'text_delta': {
      let node = live.bubbles.get(event.message_id);
      if (!node) {
        node = bubble('assistant', '');
        live.bubbles.set(event.message_id, node);
        append(node);
      }
      const stick = atBottom();
      node.textContent += event.text;
      if (stick) {
        el.stream.scrollTop = el.stream.scrollHeight;
      }
      break;
    }

    case 'thinking_delta': {
      // Its own bubble, muted: it is the model reasoning, not its answer, and
      // conflating the two would misrepresent what was said.
      let node = live.bubbles.get(event.message_id);
      if (!node) {
        node = bubble('thinking', '');
        live.bubbles.set(event.message_id, node);
        append(node);
      }
      const stickThinking = atBottom();
      node.textContent += event.text;
      if (stickThinking) {
        el.stream.scrollTop = el.stream.scrollHeight;
      }
      break;
    }

    case 'tool_start': {
      // <details> so a long result can be opened on demand without a click
      // handler, and stays collapsed until then.
      const node = document.createElement('details');
      node.className = 'tool running';

      const summary = document.createElement('summary');
      summary.textContent = event.input_preview
        ? `${event.tool_name}: ${event.input_preview}`
        : event.tool_name;
      node.append(summary);

      live.tools.set(event.tool_id, node);
      append(node);
      break;
    }

    case 'tool_end': {
      const node = live.tools.get(event.tool_id);
      if (!node) {
        append(notice(`${event.tool_name} finished`, event.is_error));
        break;
      }
      node.className = event.is_error ? 'tool failed' : 'tool done';

      const output = document.createElement('pre');
      output.className = 'tool-output';
      output.textContent = truncate(event.result || '', TOOL_OUTPUT_LIMIT);
      node.append(output);

      // A failure is the one result worth seeing without asking for it.
      node.open = Boolean(event.is_error);
      break;
    }

    case 'history': {
      // Authoritative: this is the transcript, not an addition to it. Sent on
      // connect and again whenever the machine swaps the conversation out, so
      // /clear, /new, /rewind and /resume all land here.
      //
      // Replacing rather than appending also makes a replay from the ring
      // buffer harmless; appending would duplicate the whole transcript.
      el.stream.replaceChildren();
      live.bubbles.clear();
      live.tools.clear();

      if (event.omitted > 0) {
        append(notice(`${event.omitted} earlier turn(s) not shown`));
      }
      for (const entry of event.entries || []) {
        if (entry.text) {
          append(bubble(entry.role === 'user' ? 'user' : 'assistant', entry.text));
        }
        for (const tool of entry.tools || []) {
          const node = document.createElement('div');
          node.className = 'tool done';
          node.textContent = tool;
          append(node);
        }
      }
      break;
    }

    case 'permission_request':
      showPermission(event);
      break;

    case 'mcp_approval_request':
      showMcpApproval(event);
      break;

    case 'user_question':
      showQuestion(event);
      break;

    case 'turn_complete': {
      // Close every bubble the turn opened, not just one by id.
      //
      // Deltas are keyed by content-block index (`msg-0`, `think-0`) while this
      // event is keyed by turn number, so deleting by `message_id` matched
      // nothing and the next turn's first block reused the same bubble. One
      // turn can also span several blocks, so a turn boundary ends all of them.
      live.bubbles.clear();
      el.send.disabled = false;
      // Siblings, not children: the bubble accumulates through
      // `textContent +=`, which would wipe any element inside it.
      //
      // The reason comes first because it qualifies the answer above it; the
      // figures are a footnote to both.
      const why = stopNotice(event.stop_reason);
      if (why) {
        append(why);
      }
      const spent = usageLine(event.usage);
      if (spent) {
        append(spent);
      }
      break;
    }

    case 'error':
      append(notice(event.message, true));
      break;

    case 'notice':
      // The outcome of a slash command. Unlike `status` it stays in the
      // transcript, because whoever ran the command needs the answer to
      // still be there after the next event arrives.
      append(notice(event.message, event.is_error));
      break;

    case 'status':
      // Transient by nature: it is replaced by the next one and cleared when
      // the turn ends, so it does not belong in the transcript.
      setStatus(event.message);
      break;

    case 'token_warning':
      append(
        notice(
          `Context window ${Math.round(event.pct_used * 100)}% full` +
            (event.level === 'critical' ? ' — compact now' : ' — consider /compact'),
          event.level === 'critical',
        ),
      );
      break;

    case 'session_state':
      if (event.state === 'disconnected') {
        setStatus('The machine disconnected.');
      } else if (event.state === 'processing') {
        setBusy(true);
      } else if (event.state === 'idle' || event.state === 'connected') {
        setBusy(false);
      }
      break;

    default:
      break;
  }
}

// ---------------------------------------------------------------------------
// Permission card
// ---------------------------------------------------------------------------

function showPermission(request) {
  // A request with no options cannot be answered from here. The relay forwards
  // events verbatim from whatever runner registered, so an older or mismatched
  // one could still send that; show the request without buttons rather than
  // offering a tap that does nothing.
  const answerable = Array.isArray(request.options) && request.options.length > 0;

  live.permission = answerable ? request : null;
  el.permissionTool.textContent = `${request.tool_name} needs approval`;
  el.permissionDesc.textContent = request.description || '';
  el.permissionActions.hidden = !answerable;
  el.permissionLocal.hidden = answerable;
  el.permission.hidden = false;
}

for (const button of el.permission.querySelectorAll('button[data-decision]')) {
  button.addEventListener('click', async () => {
    const request = live.permission;
    if (!request) {
      return;
    }
    // Hide first: a second tap would answer an already-settled request, and
    // the relay would forward a decision for a tool that has moved on.
    live.permission = null;
    el.permission.hidden = true;

    try {
      await api(`/api/client/sessions/${encodeURIComponent(live.sessionId)}/permission`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          request_id: request.request_id,
          tool_use_id: request.tool_use_id,
          decision: button.dataset.decision,
        }),
      });
    } catch (error) {
      reportFailure(error);
    }
  });
}

// ---------------------------------------------------------------------------
// MCP trust card
// ---------------------------------------------------------------------------

function showMcpApproval(request) {
  live.mcp = request;
  el.mcpTitle.textContent = `Trust the project MCP server '${request.server_name}'?`;
  // The command line is the decision. Falling back to the url covers an HTTP
  // server; with neither there is nothing to show beyond the name.
  el.mcpDetail.textContent = request.command
    ? `It would run: ${request.command}`
    : request.url
      ? `It would connect to: ${request.url}`
      : '';
  el.mcp.hidden = false;
}

for (const button of el.mcp.querySelectorAll('button[data-mcp]')) {
  button.addEventListener('click', async () => {
    const request = live.mcp;
    if (!request) {
      return;
    }
    // Hide first: a second tap would answer a prompt the machine has already
    // settled, and the next server in the queue would inherit the decision.
    live.mcp = null;
    el.mcp.hidden = true;

    try {
      await api(`/api/client/sessions/${encodeURIComponent(live.sessionId)}/mcp-approval`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          request_id: request.request_id,
          decision: button.dataset.mcp,
        }),
      });
    } catch (error) {
      reportFailure(error);
    }
  });
}

// ---------------------------------------------------------------------------
// Question card
// ---------------------------------------------------------------------------

function showQuestion(request) {
  live.question = request;
  el.questionText.textContent = request.question;
  el.questionInput.value = '';

  el.questionOptions.replaceChildren();
  for (const option of request.options || []) {
    const button = document.createElement('button');
    button.type = 'button';
    button.textContent = option;
    button.addEventListener('click', () => sendAnswer(option));
    el.questionOptions.append(button);
  }

  el.question.hidden = false;
}

async function sendAnswer(answer) {
  const request = live.question;
  if (!request) {
    return;
  }
  // Hide first: the turn resumes on the first answer, so a second tap would
  // be answering a question that no longer exists.
  live.question = null;
  el.question.hidden = true;

  try {
    await api(`/api/client/sessions/${encodeURIComponent(live.sessionId)}/answer`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ question_id: request.question_id, answer }),
    });
  } catch (error) {
    reportFailure(error);
  }
}

el.questionForm.addEventListener('submit', (event) => {
  event.preventDefault();
  const answer = el.questionInput.value.trim();
  if (answer) {
    sendAnswer(answer);
  }
});

// ---------------------------------------------------------------------------
// Composer
// ---------------------------------------------------------------------------

/// Matches the relay's own ceiling, so an oversized file is refused here with
/// a readable message instead of coming back as a 413.
const MAX_ATTACHMENT_BYTES = 5 * 1024 * 1024;

/** Read a file as base64 for an image, or as text for anything else. */
function readAttachment(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error(`Could not read ${file.name}`));
    reader.onload = () => {
      const result = reader.result;
      resolve({
        name: file.name,
        mime_type: file.type || 'text/plain',
        // A data URL is "data:<mime>;base64,<payload>"; the wire wants the
        // payload alone.
        content: file.type.startsWith('image/') ? String(result).split(',')[1] : String(result),
      });
    };
    if (file.type.startsWith('image/')) {
      reader.readAsDataURL(file);
    } else {
      reader.readAsText(file);
    }
  });
}

function renderAttachments() {
  if (live.attachments.length === 0) {
    el.attachments.hidden = true;
    el.attachments.textContent = '';
    return;
  }
  el.attachments.textContent = `Attached: ${live.attachments.map((a) => a.name).join(', ')}`;
  el.attachments.hidden = false;
}

el.attach.addEventListener('click', () => el.fileInput.click());

el.fileInput.addEventListener('change', async () => {
  for (const file of el.fileInput.files) {
    if (file.size > MAX_ATTACHMENT_BYTES) {
      setStatus(`${file.name} is too large (limit 5 MB)`);
      continue;
    }
    try {
      live.attachments.push(await readAttachment(file));
    } catch (error) {
      reportFailure(error);
    }
  }
  el.fileInput.value = '';
  renderAttachments();
});

// Enter sends, Shift+Enter inserts a newline. A textarea does neither on its
// own, so without this the box could only be submitted by tapping Send.
el.promptInput.addEventListener('keydown', (event) => {
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    el.promptForm.requestSubmit();
  }
});

el.promptInput.addEventListener('input', () => {
  el.promptInput.style.height = 'auto';
  el.promptInput.style.height = `${Math.min(el.promptInput.scrollHeight, 140)}px`;
});

el.promptForm.addEventListener('submit', async (event) => {
  event.preventDefault();
  const content = el.promptInput.value.trim();
  const attachments = live.attachments;
  if ((!content && attachments.length === 0) || !live.sessionId) {
    return;
  }

  el.send.disabled = true;
  el.promptInput.value = '';
  el.promptInput.style.height = 'auto';
  live.attachments = [];
  renderAttachments();

  // The relay does not echo prompts back, so the local copy is the only one.
  const names = attachments.map((a) => a.name).join(', ');
  append(bubble('user', names ? `${content}\n[${names}]` : content));

  try {
    await api(`/api/client/sessions/${encodeURIComponent(live.sessionId)}/prompt`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ content, attachments }),
    });
  } catch (error) {
    el.send.disabled = false;
    reportFailure(error);
  }
});

el.cancel.addEventListener('click', async () => {
  if (!live.sessionId) {
    return;
  }
  try {
    await api(`/api/client/sessions/${encodeURIComponent(live.sessionId)}/cancel`, {
      method: 'POST',
    });
    el.send.disabled = false;
  } catch (error) {
    reportFailure(error);
  }
});

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

openSessions().catch((error) => {
  if (error.message !== 'unauthorised') {
    show('token');
  }
});
