// Chatfenster für Besucher (läuft im iframe des Widgets, gleiche Herkunft wie der Server).
(() => {
  const KEY = "cc-chat-token";
  const $ = (id) => document.getElementById(id);
  const pageUrl = new URLSearchParams(location.search).get("page") || document.referrer || "";
  let token = null;
  try {
    token = localStorage.getItem(KEY);
  } catch {}
  let lastId = 0;
  let pollTimer = null;
  let typingSent = 0;

  function remember(t) {
    token = t;
    try {
      if (t) localStorage.setItem(KEY, t);
      else localStorage.removeItem(KEY);
    } catch {}
  }

  async function api(method, path, body) {
    const opts = { method, headers: {} };
    if (token) opts.headers["X-Chat-Token"] = token;
    if (body !== undefined) {
      opts.headers["Content-Type"] = "application/json";
      opts.body = JSON.stringify(body);
    }
    const r = await fetch(path, opts);
    const data = await r.json().catch(() => ({}));
    if (!r.ok) {
      const err = new Error(data.description || "Es ist ein Fehler aufgetreten.");
      err.status = r.status;
      throw err;
    }
    return data;
  }

  async function init() {
    clearTimeout(pollTimer);
    lastId = 0;
    $("chat-messages").replaceChildren();
    if (token) return showConversation();
    let online = false;
    try {
      online = (await api("GET", "/chat/api/status")).online;
    } catch {}
    $("chat-start").hidden = false;
    $("chat-conv").hidden = true;
    $("chat-sub").textContent = online ? "Wir sind online" : "Gerade nicht besetzt";
    $("chat-intro").textContent = online
      ? "Schreiben Sie uns – ein Mitarbeiter antwortet gleich hier im Chat."
      : "Gerade ist niemand online. Hinterlassen Sie Ihre Frage, wir antworten per E-Mail.";
  }

  function showConversation() {
    $("chat-start").hidden = true;
    $("chat-conv").hidden = false;
    $("chat-ended").hidden = true;
    $("chat-send").hidden = false;
    $("chat-end").hidden = false;
    poll();
  }

  function render(m) {
    const row = document.createElement("div");
    row.className = "msg " + (m.from_agent ? "agent" : "me");
    const bubble = document.createElement("div");
    bubble.className = "bubble";
    bubble.innerHTML = m.html; // serverseitig sicher formatiert (HTML maskiert)
    const meta = document.createElement("div");
    meta.className = "meta";
    meta.textContent = (m.from_agent ? m.author + " · " : "") + m.time;
    row.append(bubble, meta);
    $("chat-messages").append(row);
  }

  async function poll() {
    clearTimeout(pollTimer);
    try {
      const d = await api("GET", "/chat/api/poll?after=" + lastId);
      for (const m of d.messages) {
        render(m);
        lastId = Math.max(lastId, m.id);
      }
      if (d.messages.length) $("chat-messages").scrollTop = $("chat-messages").scrollHeight;
      $("chat-sub").textContent =
        d.status === "waiting"
          ? "Einen Moment – gleich ist jemand für Sie da"
          : d.status === "active"
            ? (d.agent || "Der Support") + " ist für Sie da"
            : "Chat beendet";
      $("chat-typing").hidden = !d.agent_typing;
      $("chat-typing").textContent = (d.agent || "Support") + " schreibt …";
      if (d.status === "ended") {
        $("chat-ended").hidden = false;
        $("chat-send").hidden = true;
        $("chat-end").hidden = true;
        return; // nicht weiter abfragen
      }
    } catch (e) {
      if (e.status === 404) {
        remember(null);
        return init();
      }
    }
    pollTimer = setTimeout(poll, document.hidden ? 8000 : 2500);
  }

  $("chat-start").addEventListener("submit", async (event) => {
    event.preventDefault();
    const f = event.target;
    const btn = f.querySelector("button[type=submit]");
    btn.disabled = true;
    $("start-error").textContent = "";
    try {
      const d = await api("POST", "/chat/api/start", {
        name: f.name.value,
        email: f.email.value,
        message: f.message.value,
        page_url: pageUrl,
      });
      remember(d.token);
      f.reset();
      showConversation();
    } catch (e) {
      $("start-error").textContent = e.message;
    } finally {
      btn.disabled = false;
    }
  });

  $("chat-send").addEventListener("submit", async (event) => {
    event.preventDefault();
    const area = event.target.body;
    const text = area.value.trim();
    if (!text) return;
    $("send-error").textContent = "";
    try {
      await api("POST", "/chat/api/send", { body: text });
      area.value = "";
      poll();
    } catch (e) {
      $("send-error").textContent = e.message;
    }
  });

  $("chat-send").body.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      $("chat-send").requestSubmit();
    }
  });

  $("chat-send").body.addEventListener("input", () => {
    if (Date.now() - typingSent > 3000) {
      typingSent = Date.now();
      api("POST", "/chat/api/typing").catch(() => {});
    }
  });

  $("chat-end").addEventListener("click", async () => {
    try {
      await api("POST", "/chat/api/end");
    } catch {}
    poll();
  });

  $("chat-new").addEventListener("click", () => {
    remember(null);
    init();
  });

  $("chat-close").addEventListener("click", () => {
    window.parent.postMessage({ type: "cc-chat-close" }, "*");
  });

  document.addEventListener("visibilitychange", () => {
    if (!document.hidden && token && !$("chat-conv").hidden && $("chat-ended").hidden) poll();
  });

  init();
})();
