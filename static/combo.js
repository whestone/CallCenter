// Durchsuchbare Auswahlfelder und Ticket-Vorschläge.
//
// 1. <select data-searchable>: Statt der Liste des Browsers öffnet sich ein Feld mit
//    Suche (Tippen filtert, ↑/↓ und Enter wählen, Esc schließt). Das Original-Select
//    bleibt versteckt im Formular und wird bei jeder Auswahl aktualisiert (change-Ereignis),
//    damit Speichern, „ungespeichert“-Markierung und Avatar unverändert funktionieren.
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
  // 1. Durchsuchbares Select
  // -------------------------------------------------------------------------
  // Symbole je Wert: data-icons="priority" (farbiger Punkt) oder "type" (eigenes Zeichen).
  const TYPE_ICONS = {
    question: '<circle cx="12" cy="12" r="9"/><path d="M9.1 9a3 3 0 0 1 5.8 1c0 2-3 3-3 3M12 17h.01"/>',
    incident: '<path d="M13 3 4 14h7l-1 7 9-11h-7z"/>',
    problem: '<path d="M12 3 2 20h20zM12 10v4M12 17h.01"/>',
    task: '<rect x="4" y="4" width="16" height="16" rx="3"/><path d="m8 12 3 3 5-6"/>',
  };
  function iconNode(select, opt) {
    const kind = select.dataset.icons;
    if (kind === "priority") {
      const dot = document.createElement("span");
      dot.className = `combo-dot p-${opt.value || "none"}`;
      return dot;
    }
    if (kind === "type") {
      const i = document.createElement("span");
      i.className = `combo-icon t-${opt.value || "none"}`;
      i.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true">${TYPE_ICONS[opt.value] || '<circle cx="12" cy="12" r="8" stroke-dasharray="3 3"/>'}</svg>`;
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
    label.textContent = opt.textContent.trim();
    wrap.append(label);
    return wrap;
  }

  function enhanceSelect(select) {
    if (select._combo) return;
    // Reste aus einem wiederhergestellten Seitenschnappschuss entfernen.
    if (select.nextElementSibling?.classList.contains("combo")) select.nextElementSibling.remove();
    const combo = document.createElement("span");
    combo.className = "combo";
    const trigger = document.createElement("button");
    trigger.type = "button";
    trigger.className = "combo-trigger";
    trigger.setAttribute("aria-haspopup", "listbox");
    trigger.setAttribute("aria-label", select.getAttribute("aria-label") || select.closest("label")?.firstChild?.textContent?.trim() || "Auswahl");
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
    // Kurze Listen (Typ, Priorität) ohne Suchfeld: data-searchable="plain".
    const plain = select.dataset.searchable === "plain";
    if (plain) search.classList.add("combo-search-hidden");
    panel.append(search, list);
    combo.append(trigger, panel);
    select.after(combo);
    select.classList.add("combo-native");
    select._combo = combo;

    const label = () => {
      const opt = select.selectedOptions[0];
      if (opt) trigger.replaceChildren(optionNode(select, opt, true));
      else trigger.textContent = "–";
    };
    const box = listbox(list, (entry) => {
      if (select.value !== entry.opt.value) {
        select.value = entry.opt.value;
        select.dispatchEvent(new Event("input", { bubbles: true }));
        select.dispatchEvent(new Event("change", { bubbles: true }));
      }
      label();
      close();
      trigger.focus();
    });
    function fill() {
      const q = norm(search.value.trim());
      const opts = [...select.options].filter((o) => !q || norm(o.textContent).includes(q));
      box.set(opts.map((opt) => ({ opt, node: optionNode(select, opt) })), Math.max(0, opts.findIndex((o) => o.selected)));
    }
    function open() {
      if (trigger.disabled || select.disabled) return;
      panel.hidden = false;
      combo.classList.add("open");
      search.value = "";
      fill();
      search.focus();
    }
    function close() {
      panel.hidden = true;
      combo.classList.remove("open");
    }
    trigger.addEventListener("click", () => (panel.hidden ? open() : close()));
    trigger.addEventListener("keydown", (e) => {
      if (e.key === "ArrowDown" || e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        open();
      } else if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
        // Direkt lostippen öffnet die Suche mit dem ersten Buchstaben.
        e.preventDefault();
        open();
        search.value = e.key;
        fill();
      }
    });
    search.addEventListener("input", fill);
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
    // Änderungen von außen (z. B. Makro, Zurücksetzen) übernehmen.
    select.addEventListener("change", label);
    // Gesperrtes Formular (geschlossenes Ticket) → Knopf ebenfalls gesperrt.
    if (select.closest("fieldset[disabled]") || select.disabled) trigger.disabled = true;
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
    document.querySelectorAll("select[data-searchable]").forEach(enhanceSelect);
    document.querySelectorAll("input[data-ticket-suggest]").forEach(enhanceTicketInput);
  }
  document.addEventListener("DOMContentLoaded", enhanceAll);
  document.addEventListener("htmx:afterSettle", enhanceAll);
  document.addEventListener("htmx:historyRestore", enhanceAll);
  if (document.readyState !== "loading") enhanceAll();

  // Vor dem Schnappschuss für „Zurück“ die Zusatzelemente entfernen.
  document.addEventListener("htmx:beforeHistorySave", () => {
    document.querySelectorAll("select[data-searchable]").forEach((s) => {
      s._combo?.remove();
      s._combo = null;
      s.classList.remove("combo-native");
      s.querySelectorAll("option").forEach((o) => o.toggleAttribute("selected", o.selected));
    });
    document.querySelectorAll("input[data-ticket-suggest]").forEach((i) => {
      i._suggest?.remove();
      i._suggest = null;
    });
  });
})();
