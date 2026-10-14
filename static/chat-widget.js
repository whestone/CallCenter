// Chat-Widget zum Einbinden in fremde Webseiten:
//   <script src="https://IHR-SERVER/static/chat-widget.js" async></script>
// Zeigt unten rechts eine Schaltfläche; das Chatfenster läuft in einem iframe vom Server.
(() => {
  if (window.__ccChatWidget) return;
  window.__ccChatWidget = true;
  const script = document.currentScript;
  const origin = script ? new URL(script.src).origin : location.origin;

  const style = document.createElement("style");
  style.textContent = `
    .cc-chat-btn { position: fixed; right: 20px; bottom: 20px; z-index: 2147483000; width: 58px; height: 58px;
      border-radius: 50%; border: 0; background: #1f73b7; color: #fff; font-size: 26px; cursor: pointer;
      box-shadow: 0 6px 20px rgba(0,0,0,.25); }
    .cc-chat-btn:hover { background: #144a75; }
    .cc-chat-btn:focus-visible { outline: 3px solid #fff; outline-offset: 2px; box-shadow: 0 0 0 6px #1f73b7; }
    .cc-chat-frame { position: fixed; right: 20px; bottom: 90px; z-index: 2147483000; width: 370px; height: 540px;
      max-width: calc(100vw - 32px); max-height: calc(100vh - 110px); border: 0; border-radius: 12px;
      background: #fff; box-shadow: 0 10px 40px rgba(0,0,0,.3); }
    @media (max-width: 480px) { .cc-chat-frame { right: 16px; left: 16px; width: auto; bottom: 86px; } }
  `;
  document.head.append(style);

  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "cc-chat-btn";
  btn.setAttribute("aria-label", "Chat mit dem Kundenservice öffnen");
  btn.setAttribute("aria-expanded", "false");
  btn.textContent = "💬";
  let frame = null;

  function toggle(open) {
    if (open && !frame) {
      frame = document.createElement("iframe");
      frame.className = "cc-chat-frame";
      frame.title = "Kundenservice-Chat";
      frame.src = origin + "/chat/embed?page=" + encodeURIComponent(location.href);
      document.body.append(frame);
    }
    if (frame) frame.hidden = !open;
    btn.setAttribute("aria-expanded", String(open));
    btn.textContent = open ? "✕" : "💬";
  }

  btn.addEventListener("click", () => toggle(!frame || frame.hidden));
  window.addEventListener("message", (event) => {
    if (event.origin === origin && event.data && event.data.type === "cc-chat-close") {
      toggle(false);
      btn.focus();
    }
  });
  document.body.append(btn);
})();
