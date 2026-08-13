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
  sessions: document.getElementById('view-sessions'),
  session: document.getElementById('view-session'),
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
  promptForm: document.getElementById('prompt-form'),
  promptInput: document.getElementById('prompt-input'),
  send: document.getElementById('send'),
  status: document.getElementById('session-status'),
};

/** Live session view state. Reset whenever a session is opened or left. */
const live = {
  sessionId: null,
  source: null,
  lastSeq: 0,
  bubbles: new Map(),  // message_id -> assistant bubble element
  tools: new Map(),    // tool_id -> tool row element
  permission: null,    // the request currently shown on the card
};

function show(name) {
  for (const [key, node] of Object.entries(views)) {
    node.hidden = key !== name;
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

async function openSessions() {
  leaveSession();
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
    button.addEventListener('click', () => openSession(session));

    const item = document.createElement('li');
    item.append(button);
    el.sessionList.append(item);
  }

  show('sessions');
}

el.refresh.addEventListener('click', () => {
  openSessions().catch(reportFailure);
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
  el.permission.hidden = true;
  el.stream.replaceChildren();
  el.status.hidden = true;
}

el.back.addEventListener('click', () => {
  openSessions().catch(reportFailure);
});

function openSession(session) {
  leaveSession();
  live.sessionId = session.session_id;
  el.sessionTitle.textContent = session.label || session.session_id;
  show('session');
  connectStream();
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
    setStatus('Reconnecting…');
    setTimeout(() => {
      if (live.sessionId) {
        connectStream();
      }
    }, 2000);
  });

  source.addEventListener('open', () => {
    el.status.hidden = true;
  });
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

    case 'tool_start': {
      const node = document.createElement('div');
      node.className = 'tool running';
      node.textContent = event.input_preview
        ? `${event.tool_name}: ${event.input_preview}`
        : event.tool_name;
      live.tools.set(event.tool_id, node);
      append(node);
      break;
    }

    case 'tool_end': {
      const node = live.tools.get(event.tool_id);
      if (node) {
        node.className = event.is_error ? 'tool failed' : 'tool done';
      } else {
        append(notice(`${event.tool_name} finished`, event.is_error));
      }
      break;
    }

    case 'permission_request':
      showPermission(event);
      break;

    case 'turn_complete':
      // A new turn must not append to the finished bubble.
      live.bubbles.delete(event.message_id);
      el.send.disabled = false;
      break;

    case 'error':
      append(notice(event.message, true));
      break;

    case 'session_state':
      if (event.state === 'disconnected') {
        setStatus('The machine disconnected.');
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
  live.permission = request;
  el.permissionTool.textContent = `${request.tool_name} needs approval`;
  el.permissionDesc.textContent = request.description || '';
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
// Composer
// ---------------------------------------------------------------------------

el.promptInput.addEventListener('input', () => {
  el.promptInput.style.height = 'auto';
  el.promptInput.style.height = `${Math.min(el.promptInput.scrollHeight, 140)}px`;
});

el.promptForm.addEventListener('submit', async (event) => {
  event.preventDefault();
  const content = el.promptInput.value.trim();
  if (!content || !live.sessionId) {
    return;
  }

  el.send.disabled = true;
  el.promptInput.value = '';
  el.promptInput.style.height = 'auto';
  // The relay does not echo prompts back, so the local copy is the only one.
  append(bubble('user', content));

  try {
    await api(`/api/client/sessions/${encodeURIComponent(live.sessionId)}/prompt`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ content }),
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
