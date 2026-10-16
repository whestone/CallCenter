// Anhänge im Popup: Vorschau für Bilder, PDFs und Text, Blättern durch alle
// Anhänge der Seite (Pfeile, Tastatur ← →), Herunterladen und neues Fenster.
// Links: <a data-attachment data-kind="image|pdf|text|file" data-name="…" data-size="…" href="…">
// Strg-/Mittelklick behält das normale Verhalten (neuer Tab).
(() => {
  let dialog = null;
  let items = [];
  let index = 0;

  const icon = (d) => `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="${d}"/></svg>`;

  function build() {
    dialog = document.createElement("dialog");
    dialog.className = "lightbox";
    dialog.setAttribute("aria-label", "Anhang");
    dialog.innerHTML = `
      <header class="lightbox-head">
        <div class="lightbox-title"><strong data-lb-name></strong><span class="muted small" data-lb-meta></span></div>
        <a class="icon-btn" data-lb-download title="Herunterladen" aria-label="Herunterladen" download>${icon("M12 4v11M7 10l5 5 5-5M5 20h14")}</a>
        <a class="icon-btn" data-lb-open title="In neuem Tab öffnen" aria-label="In neuem Tab öffnen" target="_blank" rel="noopener">${icon("M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5")}</a>
        <button type="button" class="icon-btn" data-lb-close title="Schließen (Esc)" aria-label="Schließen">${icon("M6 6l12 12M18 6 6 18")}</button>
      </header>
      <div class="lightbox-stage">
        <button type="button" class="lightbox-nav prev" data-lb-prev title="Vorheriger Anhang (←)" aria-label="Vorheriger Anhang">${icon("M15 6l-6 6 6 6")}</button>
        <div class="lightbox-content" data-lb-content></div>
        <button type="button" class="lightbox-nav next" data-lb-next title="Nächster Anhang (→)" aria-label="Nächster Anhang">${icon("M9 6l6 6-6 6")}</button>
      </div>
      <footer class="lightbox-strip" data-lb-strip></footer>`;
    document.body.append(dialog);
    dialog.addEventListener("click", (e) => {
      if (e.target === dialog || e.target.closest("[data-lb-close]")) dialog.close();
      else if (e.target.closest("[data-lb-prev]")) show(index - 1);
      else if (e.target.closest("[data-lb-next]")) show(index + 1);
      else {
        const thumb = e.target.closest("[data-lb-index]");
        if (thumb) show(Number(thumb.dataset.lbIndex));
      }
    });
    dialog.addEventListener("keydown", (e) => {
      if (e.key === "ArrowLeft") { e.preventDefault(); show(index - 1); }
      if (e.key === "ArrowRight") { e.preventDefault(); show(index + 1); }
    });
    dialog.addEventListener("close", () => {
      dialog.querySelector("[data-lb-content]").replaceChildren();
    });
  }

  function collect() {
    // Jede Datei nur einmal (ein Anhang kann im Gespräch und in der Seitenleiste stehen).
    const seen = new Set();
    return [...document.querySelectorAll("a[data-attachment]")].filter((a) => {
      if (seen.has(a.href)) return false;
      seen.add(a.href);
      return true;
    }).map((a) => ({ href: a.href, kind: a.dataset.kind || "file", name: a.dataset.name || a.textContent.trim(), size: a.dataset.size || "" }));
  }

  function el(tag, attrs = {}, text) {
    const n = document.createElement(tag);
    Object.entries(attrs).forEach(([k, v]) => n.setAttribute(k, v));
    if (text) n.textContent = text;
    return n;
  }

  function show(i) {
    if (!items.length) return;
    index = (i + items.length) % items.length;
    const it = items[index];
    dialog.querySelector("[data-lb-name]").textContent = it.name;
    dialog.querySelector("[data-lb-meta]").textContent = [it.size, items.length > 1 ? `${index + 1} von ${items.length}` : ""].filter(Boolean).join(" · ");
    dialog.querySelector("[data-lb-download]").href = it.href;
    dialog.querySelector("[data-lb-download]").setAttribute("download", it.name);
    dialog.querySelector("[data-lb-open]").href = it.href;
    const content = dialog.querySelector("[data-lb-content]");
    let view;
    if (it.kind === "image") {
      view = el("img", { src: it.href, alt: it.name });
    } else if (it.kind === "pdf") {
      view = el("iframe", { src: it.href, title: it.name, class: "lightbox-frame" });
    } else if (it.kind === "text") {
      view = el("pre", { class: "lightbox-text" }, "Wird geladen …");
      const shown = index;
      fetch(it.href, { credentials: "same-origin" })
        .then((r) => (r.ok ? r.text() : Promise.reject()))
        .then((t) => {
          if (index !== shown) return;
          view.textContent = t.length > 200000 ? `${t.slice(0, 200000)}\n…` : t;
        })
        .catch(() => { view.textContent = "Die Datei konnte nicht geladen werden."; });
    } else {
      view = el("div", { class: "lightbox-file" });
      view.innerHTML = icon("M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8zM14 3v5h5");
      view.append(el("strong", {}, it.name), el("span", { class: "muted" }, "Für diese Datei gibt es keine Vorschau."));
      const dl = el("a", { class: "btn primary", href: it.href, download: it.name }, "Herunterladen");
      view.append(dl);
    }
    content.replaceChildren(view);
    dialog.querySelectorAll("[data-lb-prev], [data-lb-next]").forEach((b) => (b.hidden = items.length < 2));
    const strip = dialog.querySelector("[data-lb-strip]");
    strip.hidden = items.length < 2;
    strip.replaceChildren(...items.map((x, n) => {
      const b = el("button", { type: "button", class: `lightbox-thumb${n === index ? " active" : ""}`, "data-lb-index": String(n), title: x.name, "aria-label": x.name });
      if (x.kind === "image") b.append(el("img", { src: x.href, alt: "" }));
      else b.append(el("span", {}, x.kind === "pdf" ? "PDF" : x.kind === "text" ? "TXT" : (x.name.split(".").pop() || "").slice(0, 4).toUpperCase()));
      return b;
    }));
    strip.querySelector(".active")?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }

  document.addEventListener("click", (e) => {
    const link = e.target.closest?.("a[data-attachment]");
    if (!link || e.ctrlKey || e.metaKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    if (!dialog || !dialog.isConnected) build();
    items = collect();
    const start = items.findIndex((x) => x.href === link.href);
    if (!dialog.open) dialog.showModal();
    show(start < 0 ? 0 : start);
  });
})();
