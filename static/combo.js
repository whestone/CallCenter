// Durchsuchbare Auswahlfelder und Ticket-Vorschläge.
//
// 1. Jedes <select> (außer multiple/size>1 und data-native): eigene Auswahlliste mit
//    Symbolen (Status, Priorität, Typ, Avatare) und – ab 8 Einträgen oder mit
//    data-searchable – Suchfeld (Tippen filtert, ↑/↓ und Enter wählen, Esc schließt).
//    Das Original-Select bleibt unsichtbar im Formular und wird bei jeder Auswahl
//    aktualisiert (input/change), damit Speichern, Filter, „ungespeichert“-Markierung,
//    onchange-Handler und Avatar unverändert funktionieren.
// 2. <input data-ticket-suggest data-exclude="ID" [data-open]>: zeigt beim Tippen passende
//    Tickets (Nummer, Betreff, Kunde); ohne Text die übrigen Tickets desselben Kunden.
(() => {
  const norm = (s) => s.toLocaleLowerCase("de").normalize("NFD").replace(/[̀-ͯ]/g, "");

  // Gemeinsame Listen-Logik (Tastatur, Markierung, Auswahl per Maus)
  function listbox(list, onPick) {
    let items = [];
    let active = -1;
    const paint = () => list.querySelectorAll("li").forEach((li, i) => {
      li.classList.toggle("active", i === active);
      if (i === active) li.scrollIntoView({ block: "nearest" });
    });
    return {
      set(entries, preselect = -1) {
        items = entries;
        active = preselect;
        list.replaceChildren(...entries.map((e, i) => {
          const li = document.createElement("li");
          li.setAttribute("role", "option");
          li.append(e.node);
          li.addEventListener("mousedown", (ev) => {
            ev.preventDefault();
            onPick(items[i]);
          });
          return li;
        }));
        if (!entries.length) {
          const li = document.createElement("li");
          li.className = "combo-empty";
          li.textContent = "Keine Treffer";
          list.append(li);
        }
        paint();
      },
      key(event) {
        const n = items.length;
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault();
          if (!n) return true;
          active = event.key === "ArrowDown" ? (active + 1) % n : (active - 1 + n) % n;
          paint();
          return true;
        }
        if (event.key === "Enter") {
          event.preventDefault();
          if (active >= 0 && items[active]) onPick(items[active]);
          else if (n === 1) onPick(items[0]);
          return true;
        }
        return false;
      },
    };
  }

  // -------------------------------------------------------------------------
  // 1. Auswahlfelder (global für jedes <select>)
  // -------------------------------------------------------------------------
  // Symbole je Wert: Priorität und Status als Farbpunkt, Typ als eigenes Zeichen.
  // Ergibt sich aus dem Feldnamen oder ausdrücklich über data-icons="priority|status|type".
  const TYPE_ICONS = {
    question: '<circle cx="12" cy="12" r="9"/><path d="M9.1 9a3 3 0 0 1 5.8 1c0 2-3 3-3 3M12 17h.01"/>',
    incident: '<path d="M13 3 4 14h7l-1 7 9-11h-7z"/>',
    problem: '<path d="M12 3 2 20h20zM12 10v4M12 17h.01"/>',
    task: '<rect x="4" y="4" width="16" height="16" rx="3"/><path d="m8 12 3 3 5-6"/>',
  };
  const ICON_BY_NAME = { priority: "priority", status: "status", ticket_type: "type" };
  const PRIORITIES = ["low", "normal", "high", "urgent"];
  const STATUSES = ["new", "open", "pending", "hold", "solved", "closed"];
  const TYPES = Object.keys(TYPE_ICONS);

  function iconKind(select) {
    return select.dataset.icons || ICON_BY_NAME[select.name] || "";
  }
  function iconNode(select, opt) {
    const kind = iconKind(select);
    const v = opt.value;
    if (kind === "priority" && (!v || PRIORITIES.includes(v))) {
      const dot = document.createElement("span");
      dot.className = `combo-dot p-${v || "none"}`;
      return dot;
    }
    if (kind === "status" && (!v || STATUSES.includes(v))) {
      const dot = document.createElement("span");
      dot.className = `combo-dot st-${v || "none"}`;
      return dot;
    }
    if (kind === "type" && (!v || TYPES.includes(v))) {
      const i = document.createElement("span");
      i.className = `combo-icon t-${v || "none"}`;
      i.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true">${TYPE_ICONS[v] || '<circle cx="12" cy="12" r="8" stroke-dasharray="3 3"/>'}</svg>`;
      return i;
    }
    return null;
  }

  function optionNode(select, opt, inTrigger = false) {
    const wrap = document.createElement("span");
    wrap.className = "combo-option";
    const icon = iconNode(select, opt);
    if (icon) wrap.append(icon);
    // Im Feld selbst steht der Avatar schon daneben (siehe .select-avatar).
    if (!inTrigger && select.hasAttribute("data-avatar-select")) {
      const av = document.createElement("span");
      av.className = "avatar xxs" + (opt.value ? "" : " is-empty");
      if (opt.dataset.photo) {
        av.classList.add("has-photo");
        const img = document.createElement("img");
        img.src = opt.dataset.photo;
        img.alt = "";
        av.append(img);
      } else if (opt.value) {
        av.textContent = opt.textContent.trim().split(/\s+/).slice(0, 2).map((w) => w[0]).join("").toUpperCase();
      }
      wrap.append(av);
    }
    const label = document.createElement("span");
    label.className = "combo-label";
    label.textContent = opt.textContent.trim();
    wrap.append(label);
    return wrap;
  }

  // Suchfeld: data-searchable="plain" nie, data-searchable (sonst) immer, ohne Angabe ab 8 Einträgen.
  function wantsSearch(select) {
    if (!select.hasAttribute("data-searchable")) return select.options.length > 7;
    return select.dataset.searchable !== "plain";
  }

  function skip(select) {
    return select.multiple || select.size > 1 || select.hasAttribute("data-native") || !!select.closest("[data-native]");
  }

  function enhanceSelect(select) {
    if (select._combo || skip(select)) return;
    // Reste aus einem wiederhergestellten Schnappschuss/Popup entfernen.
    if (select.nextElementSibling?.classList.contains("combo")) select.nextElementSibling.remove();
    const combo = document.createElement("span");
    combo.className = "combo";
    // Breite wie das Original: volle Breite in Formularfeldern, sonst so breit wie der Inhalt.
    const parent = select.parentElement;
    const visibleWidth = select.offsetWidth;
    const block = select.hasAttribute("data-block") ||
      (visibleWidth ? visibleWidth >= (parent?.clientWidth || 0) - 4 : !!select.closest("label"));
    if (!block) {
      combo.classList.add("combo-inline");
      if (visibleWidth) combo.style.minWidth = `${visibleWidth}px`;
    }
    const trigger = document.createElement("button");
    trigger.type = "button";
    trigger.className = "combo-trigger";
    trigger.setAttribute("aria-haspopup", "listbox");
    trigger.setAttribute("aria-label", select.getAttribute("aria-label") || select.closest("label")?.firstChild?.textContent?.trim() || "Auswahl");
    if (select.title) trigger.title = select.title;
    const panel = document.createElement("div");
    panel.className = "combo-panel";
    panel.hidden = true;
    const search = document.createElement("input");
    search.type = "search";
    search.className = "combo-search";
    search.placeholder = "Suchen …";
    search.autocomplete = "off";
    search.setAttribute("aria-label", "Auswahl durchsuchen");
    const list = document.createElement("ul");
    list.className = "combo-list";
    list.setAttribute("role", "listbox");
    panel.append(search, list);
    combo.append(trigger, panel);
    select.after(combo);
    select.classList.add("combo-native");
    select.tabIndex = -1;
    select._combo = combo;

    const label = () => {
      const opt = select.selectedOptions[0];
      if (opt) trigger.replaceChildren(optionNode(select, opt, true));
      else trigger.textContent = "–";
      combo.classList.toggle("has-value", select.value !== "");
      trigger.disabled = select.disabled || !!select.closest("fieldset[disabled]");
    };
    const box = listbox(list, (entry) => {
      close();
      if (select.value !== entry.opt.value) {
        select.value = entry.opt.value;
        label();
        select.dispatchEvent(new Event("input", { bubbles: true }));
        select.dispatchEvent(new Event("change", { bubbles: true }));
      }
      if (trigger.isConnected) trigger.focus();
    });
    function fill() {
      const q = norm(search.value.trim());
      const opts = [...select.options].filter((o) => !o.hidden && (!q || norm(o.textContent).includes(q)));
      box.set(opts.map((opt) => ({ opt, node: optionNode(select, opt) })), Math.max(0, opts.findIndex((o) => o.selected)));
    }
    // Die offene Liste liegt fest über der Seite (position: fixed), damit Tabellen,
    // Karten oder Popups mit Scrollbereich sie nicht abschneiden. Sie folgt beim
    // Scrollen dem Feld und klappt nach oben auf, wenn unten kein Platz ist.
    function place() {
      if (panel.hidden) return;
      const r = trigger.getBoundingClientRect();
      const gap = 4;
      const below = window.innerHeight - r.bottom - gap - 8;
      const above = r.top - gap - 8;
      const wanted = Math.min(panel.scrollHeight, 320);
      const up = below < Math.min(wanted, 220) && above > below;
      const room = Math.max(120, up ? above : below);
      list.style.maxHeight = `${Math.max(80, room - (search.classList.contains("combo-search-hidden") ? 12 : 56))}px`;
      const width = Math.max(r.width, combo.classList.contains("combo-inline") ? 220 : 0);
      const left = Math.min(Math.max(8, r.left), window.innerWidth - width - 8);
      panel.style.width = `${width}px`;
      panel.style.left = `${left}px`;
      panel.style.top = up ? `${r.top - gap - panel.offsetHeight}px` : `${r.bottom + gap}px`;
      combo.classList.toggle("combo-up", up);
      // Bei einem umgebenden Element mit transform (z. B. Popup) bezieht sich „fixed“
      // auf dieses – die Abweichung einmal messen und ausgleichen.
      const p = panel.getBoundingClientRect();
      const dx = p.left - left;
      const dy = p.top - (up ? r.top - gap - p.height : r.bottom + gap);
      if (Math.abs(dx) > 1) panel.style.left = `${left - dx}px`;
      if (Math.abs(dy) > 1) panel.style.top = `${parseFloat(panel.style.top) - dy}px`;
    }
    function onScroll(e) {
      // Scrollen innerhalb der Liste selbst nicht beachten.
      if (panel.contains(e.target)) return;
      place();
    }
    function open() {
      if (trigger.disabled) return;
      label();
      search.classList.toggle("combo-search-hidden", !wantsSearch(select));
      panel.hidden = false;
      combo.classList.add("open");
      search.value = "";
      fill();
      place();
      window.addEventListener("scroll", onScroll, true);
      window.addEventListener("resize", place);
      search.focus();
    }
    function close() {
      panel.hidden = true;
      combo.classList.remove("open");
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", place);
    }
    trigger.addEventListener("click", () => (panel.hidden ? open() : close()));
    trigger.addEventListener("keydown", (e) => {
      if (e.key === "ArrowDown" || e.key === "ArrowUp" || e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        open();
      } else if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
        // Direkt lostippen öffnet die Suche mit dem ersten Buchstaben.
        e.preventDefault();
        open();
        search.value = e.key;
        fill();
        place();
      }
    });
    search.addEventListener("input", () => {
      fill();
      place();
    });
    search.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        close();
        trigger.focus();
        return;
      }
      if (e.key === "Tab") return close();
      box.key(e);
    });
    document.addEventListener("mousedown", (e) => {
      if (!panel.hidden && !combo.contains(e.target)) close();
    });
    // Fokus auf das (unsichtbare) Original – z. B. Klick aufs Label – geht an den Knopf.
    select.addEventListener("focus", () => trigger.focus());
    // Änderungen von außen (Makro, Skript, Zurücksetzen, neue Optionen) übernehmen.
    select.addEventListener("change", label);
    select.addEventListener("input", label);
    select.form?.addEventListener("reset", () => setTimeout(label, 0));
    new MutationObserver(label).observe(select, { childList: true, subtree: true, attributes: true, attributeFilter: ["disabled"] });
    label();
  }

  // -------------------------------------------------------------------------
  // 2. Ticket-Vorschläge
  // -------------------------------------------------------------------------
  function ticketNode(t) {
    const wrap = document.createElement("span");
    wrap.className = "combo-ticket";
    const pill = document.createElement("span");
    pill.className = `status-badge s-${t.status}`;
    pill.textContent = t.status_label;
    const text = document.createElement("span");
    text.className = "combo-ticket-text";
    const title = document.createElement("strong");
    title.textContent = `#${t.id} ${t.subject}`;
    const meta = document.createElement("small");
    meta.textContent = t.same_requester ? `${t.requester} · gleicher Kunde` : t.requester;
    text.append(title, meta);
    wrap.append(pill, text);
    return wrap;
  }

  function enhanceTicketInput(input) {
    if (input._suggest) return;
    if (input.nextElementSibling?.classList.contains("combo-list")) input.nextElementSibling.remove();
    const wrap = input.parentElement;
    wrap.classList.add("combo-anchor");
    const list = document.createElement("ul");
    list.className = "combo-list combo-floating";
    list.setAttribute("role", "listbox");
    list.hidden = true;
    input.after(list);
    input._suggest = list;
    input.setAttribute("role", "combobox");
    input.setAttribute("aria-autocomplete", "list");
    const box = listbox(list, (entry) => {
      input.value = String(entry.t.id);
      list.hidden = true;
      input.dispatchEvent(new Event("input", { bubbles: true, composed: true }));
      input.dataset.picked = "1";
    });
    let timer;
    let seq = 0;
    async function load() {
      const n = ++seq;
      const params = new URLSearchParams({ q: input.value.trim(), exclude: input.dataset.exclude || "" });
      if (input.hasAttribute("data-open")) params.set("open", "1");
      const found = await fetch("/tickets/suggest?" + params).then((r) => (r.ok ? r.json() : [])).catch(() => []);
      if (n !== seq || document.activeElement !== input) return;
      if (!found.length && !input.value.trim()) {
        list.hidden = true;
        return;
      }
      box.set(found.map((t) => ({ t, node: ticketNode(t) })));
      list.hidden = false;
    }
    input.addEventListener("input", () => {
      if (input.dataset.picked) {
        delete input.dataset.picked;
        return;
      }
      clearTimeout(timer);
      timer = setTimeout(load, 150);
    });
    input.addEventListener("focus", load);
    input.addEventListener("blur", () => setTimeout(() => (list.hidden = true), 100));
    input.addEventListener("keydown", (e) => {
      if (list.hidden) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        list.hidden = true;
        return;
      }
      // Enter ohne markierten Vorschlag schickt das Formular normal ab.
      if (e.key === "Enter" && !list.querySelector("li.active")) return;
      box.key(e);
    });
  }

  function enhanceAll() {
    document.querySelectorAll("select").forEach(enhanceSelect);
    document.querySelectorAll("input[data-ticket-suggest]").forEach(enhanceTicketInput);
  }
  document.addEventListener("DOMContentLoaded", enhanceAll);
  document.addEventListener("htmx:afterSettle", enhanceAll);
  document.addEventListener("htmx:historyRestore", enhanceAll);
  if (document.readyState !== "loading") enhanceAll();
  // Später eingefügte Felder (Regel-Editor, Popups, nachgeladene Teile) ebenfalls umstellen.
  const relevant = (n) => n.nodeType === 1 && (n.matches("select, input[data-ticket-suggest]") || n.querySelector("select, input[data-ticket-suggest]"));
  let pending = false;
  new MutationObserver((records) => {
    if (pending || !records.some((r) => [...r.addedNodes].some(relevant))) return;
    pending = true;
    queueMicrotask(() => {
      pending = false;
      enhanceAll();
    });
  }).observe(document.documentElement, { childList: true, subtree: true });

  // Vor dem Schnappschuss für „Zurück“ die Zusatzelemente entfernen.
  document.addEventListener("htmx:beforeHistorySave", () => {
    document.querySelectorAll("select.combo-native").forEach((s) => {
      s._combo?.remove();
      s._combo = null;
      s.classList.remove("combo-native");
      s.removeAttribute("tabindex");
      s.querySelectorAll("option").forEach((o) => o.toggleAttribute("selected", o.selected));
    });
    document.querySelectorAll("input[data-ticket-suggest]").forEach((i) => {
      i._suggest?.remove();
      i._suggest = null;
    });
  });
})();
