// Kleine Helfer für die Agenten-Oberfläche (Ereignis-Delegation, übersteht htmx-Swaps).

document.addEventListener("click", (event) => {
  const hit = event.target.closest("[data-pick-requester]");
  if (hit) {
    document.getElementById("requester_id").value = hit.dataset.pickRequester;
    document.getElementById("requester-picked-label").textContent = hit.dataset.label;
    document.getElementById("requester-picked").hidden = false;
    document.getElementById("requester-fields").hidden = true;
    document.getElementById("requester-results").innerHTML = "";
    return;
  }
  // "als Wartend/Gelöst": Status-Auswahl mitsetzen, damit der Wert sicher übertragen wird.
  const submitAs = event.target.closest("button[name=submit_as]");
  if (submitAs) {
    const status = submitAs.form && submitAs.form.querySelector("select[name=status]");
    if (status) status.value = submitAs.value;
    return;
  }
  if (event.target.closest("[data-clear-requester]")) {
    document.getElementById("requester_id").value = "";
    document.getElementById("requester-picked").hidden = true;
    document.getElementById("requester-fields").hidden = false;
  }
});

// Massenbearbeitung: "alle auswählen" und Zähler der markierten Tickets.
function updateBulkCount() {
  const boxes = document.querySelectorAll("input[name=ticket_id]");
  const checked = [...boxes].filter((b) => b.checked).length;
  const counter = document.querySelector("[data-bulk-count]");
  if (counter) counter.textContent = checked + " ausgewählt";
  const all = document.querySelector("[data-select-all]");
  if (all) {
    all.checked = boxes.length > 0 && checked === boxes.length;
    all.indeterminate = checked > 0 && checked < boxes.length;
  }
}

document.addEventListener("change", (event) => {
  if (event.target.matches("[data-select-all]")) {
    document.querySelectorAll("input[name=ticket_id]").forEach((b) => (b.checked = event.target.checked));
  }
  if (event.target.matches("[data-select-all], input[name=ticket_id]")) updateBulkCount();
});

// Ungespeicherte Änderungen im Ticket: Felder markieren, "Absenden" hervorheben,
// beim Verlassen warnen (echter Seitenwechsel und htmx-Navigation).
const TICKET_FORM = "form.ticket-layout";

function fieldChanged(el) {
  if (el.type === "hidden" || el.type === "submit" || el.type === "button" || el.name === "public") return false;
  // Die mitlaufende Zeiterfassung zählt nicht als Änderung.
  if (el.matches("[data-timer]")) return false;
  if (el.type === "checkbox" || el.type === "radio") return el.checked !== el.defaultChecked;
  if (el.tagName === "SELECT") {
    const opts = [...el.options];
    // Ohne vorausgewählte Option zeigt der Browser die erste an.
    const initial = opts.findIndex((o) => o.defaultSelected);
    return el.selectedIndex !== (initial === -1 ? 0 : initial);
  }
  return el.value !== el.defaultValue;
}

function updateDirty(form) {
  let dirty = false;
  for (const el of form.elements) {
    // Versteckte Felder (orig_*) teilen sich das Label mit der sichtbaren Eingabe.
    if (!el.name || el.type === "hidden") continue;
    const changed = fieldChanged(el);
    dirty ||= changed;
    el.closest("label")?.classList.toggle("is-dirty", changed && el.name !== "comment");
  }
  form.classList.toggle("has-changes", dirty);
  return dirty;
}

const dirtyTicketForm = () => {
  const form = document.querySelector(TICKET_FORM);
  return form && !form.dataset.submitting && updateDirty(form) ? form : null;
};

for (const type of ["input", "change"]) {
  document.addEventListener(type, (event) => {
    const form = event.target.closest?.(TICKET_FORM) || event.target.form;
    if (form?.matches(TICKET_FORM)) updateDirty(form);
  });
}

document.addEventListener("submit", (event) => {
  if (event.target.matches(TICKET_FORM)) event.target.dataset.submitting = "1";
});

window.addEventListener("beforeunload", (event) => {
  if (dirtyTicketForm()) event.preventDefault();
});

// Eigener Bestätigungsdialog statt window.confirm(): Native Dialoge werden in
// eingebetteten Browsern (z. B. App-Vorschau) teils unterdrückt – dann passierte
// beim Klick auf "Löschen"/"Widerrufen" scheinbar nichts.
function askConfirm(message, okLabel) {
  return new Promise((resolve) => {
    let dlg = document.getElementById("cc-confirm");
    if (!dlg) {
      dlg = document.createElement("dialog");
      dlg.id = "cc-confirm";
      dlg.className = "confirm-dialog";
      dlg.innerHTML =
        '<p class="confirm-text"></p><div class="confirm-actions">' +
        '<button type="button" class="btn" data-answer="no">Abbrechen</button>' +
        '<button type="button" class="btn primary" data-answer="yes"></button></div>';
      document.body.append(dlg);
    }
    dlg.querySelector(".confirm-text").textContent = message;
    dlg.querySelector('[data-answer="yes"]').textContent = okLabel;
    const done = (answer) => {
      dlg.close();
      resolve(answer);
    };
    dlg.querySelector('[data-answer="yes"]').onclick = () => done(true);
    dlg.querySelector('[data-answer="no"]').onclick = () => done(false);
    dlg.oncancel = (e) => {
      e.preventDefault();
      done(false);
    };
    dlg.showModal();
    dlg.querySelector('[data-answer="no"]').focus();
  });
}

// Läuft vor jeder htmx-Anfrage: Rückfragen aus hx-confirm und Warnung bei
// ungespeicherten Änderungen im Ticket.
document.addEventListener("htmx:confirm", (event) => {
  const question = event.detail.question;
  const form = dirtyTicketForm();
  const source = event.detail.elt;
  // Nur echte Navigation (Links, Formulare) verlässt das Ticket – Hintergrund-Abfragen
  // wie Anruf-Banner, Glocke oder Anwesenheit nicht. Absenden des Ticketformulars ist erlaubt.
  const navigates = source.matches("a[href], form, button, input[type=submit]");
  const leavesDirtyTicket = form && navigates && source !== form && source.form !== form;
  if (!question && !leavesDirtyTicket) return;
  event.preventDefault();
  const message = question || "Ihre Änderungen am Ticket sind noch nicht gespeichert. Trotzdem fortfahren und verwerfen?";
  askConfirm(message, question ? "Ja, fortfahren" : "Änderungen verwerfen").then((ok) => {
    if (!ok) return;
    if (leavesDirtyTicket) form.dataset.submitting = "1";
    event.detail.issueRequest(true);
  });
});

// Antwortbereich: Makro sofort anwenden, Dateien sofort hochladen,
// Sichtbarkeit der Anhänge folgt "Öffentliche Antwort / Interne Notiz".
document.addEventListener("change", (event) => {
  const el = event.target;
  if (el.matches("select[data-auto-submit]") && el.value) {
    el.form.requestSubmit();
    el.value = ""; // falls der Wechsel abgebrochen wird
  }
  if (el.matches("input[data-auto-upload]") && el.files.length) {
    el.form.requestSubmit();
  }
  if (el.matches(".composer-tabs input[name=public]")) {
    const hidden = document.querySelector("[data-upload-public]");
    if (hidden) hidden.value = el.value;
    const btn = document.querySelector(".attach-btn");
    if (btn) btn.title = el.value === "0" ? "Wird als interne Notiz hochgeladen" : "Wird als öffentliche Antwort hochgeladen";
  }
});

// Hilfe-Artikel per Klick als Link in die Antwort übernehmen.
document.addEventListener("click", (event) => {
  const btn = event.target.closest("[data-insert]");
  if (!btn) return;
  const area = document.querySelector("textarea[name=comment]");
  if (!area) return;
  area.value = (area.value.trim() ? area.value.trimEnd() + "\n\n" : "") + btn.dataset.insert;
  area.dispatchEvent(new Event("input", { bubbles: true }));
  area.dispatchEvent(new Event("cc:replace"));
  (area._editor || area).focus();
});

// Status-Menü am Absenden-Button schließen, wenn daneben geklickt wird.
document.addEventListener("click", (event) => {
  document.querySelectorAll("details.split-menu[open]").forEach((menu) => {
    if (!menu.contains(event.target)) menu.removeAttribute("open");
  });
});

// Strg+Enter schickt das Antwortformular ab.
document.addEventListener("keydown", (event) => {
  if (event.key === "Enter" && (event.ctrlKey || event.metaKey) && event.target.matches("textarea[name=comment]")) {
    event.preventDefault();
    event.target.form.requestSubmit();
  }
});

// ---------------------------------------------------------------------------
// Kollisionserkennung: „schreibt gerade“ (wird von der Anwesenheitsabfrage gelesen)
// ---------------------------------------------------------------------------
let lastTyped = 0;
document.addEventListener("input", (event) => {
  if (event.target.matches("textarea[name=comment]")) lastTyped = Date.now();
});
window.ccComposing = () => (Date.now() - lastTyped < 10000 ? 1 : 0);

// ---------------------------------------------------------------------------
// Zeiterfassung: läuft mit, solange das Ticket sichtbar ist; manuell überschreibbar
// ---------------------------------------------------------------------------
function fmtTimer(total) {
  const h = Math.floor(total / 3600);
  const m = String(Math.floor((total % 3600) / 60)).padStart(2, "0");
  const s = String(total % 60).padStart(2, "0");
  return h ? `${h}:${m}:${s}` : `${m}:${s}`;
}
setInterval(() => {
  const el = document.querySelector("[data-timer]");
  if (!el || el.dataset.touched || document.hidden) return;
  const elapsed = Number(el.dataset.elapsed || 0) + 1;
  el.dataset.elapsed = elapsed;
  el.value = fmtTimer(elapsed);
}, 1000);
document.addEventListener("input", (event) => {
  if (event.target.matches("[data-timer]")) event.target.dataset.touched = "1";
});

// ---------------------------------------------------------------------------
// Formatierleiste im Antwortfeld
// ---------------------------------------------------------------------------
function notifyInput(area) {
  area.dispatchEvent(new Event("input", { bubbles: true }));
}
function wrapSelection(area, before, after, placeholder) {
  const { selectionStart: s, selectionEnd: e, value: v } = area;
  const sel = v.slice(s, e) || placeholder;
  area.value = v.slice(0, s) + before + sel + after + v.slice(e);
  area.focus();
  area.setSelectionRange(s + before.length, s + before.length + sel.length);
  notifyInput(area);
}
function prefixLines(area, prefix) {
  const { selectionStart: s, selectionEnd: e, value: v } = area;
  const start = v.lastIndexOf("\n", s - 1) + 1;
  let end = v.indexOf("\n", e);
  if (end < 0) end = v.length;
  const text = v.slice(start, end).split("\n").map((line, i) => prefix(i) + line).join("\n");
  area.value = v.slice(0, start) + text + v.slice(end);
  area.focus();
  area.setSelectionRange(start + text.length, start + text.length);
  notifyInput(area);
}
function applyFormat(area, fmt) {
  switch (fmt) {
    case "bold": return wrapSelection(area, "**", "**", "fett");
    case "italic": return wrapSelection(area, "*", "*", "kursiv");
    case "code": return wrapSelection(area, "`", "`", "Code");
    case "link": return wrapSelection(area, "[", "](https://)", "Linktext");
    case "ul": return prefixLines(area, () => "- ");
    case "ol": return prefixLines(area, (i) => `${i + 1}. `);
  }
}
document.addEventListener("click", (event) => {
  const btn = event.target.closest("[data-fmt]");
  if (!btn) return;
  const area = document.querySelector("textarea[name=comment]");
  if (area?._editor) window.ccEditorFormat(area._editor, btn.dataset.fmt);
  else if (area) applyFormat(area, btn.dataset.fmt);
});
document.addEventListener("keydown", (event) => {
  if (!(event.ctrlKey || event.metaKey) || !event.target.matches("textarea[name=comment]")) return;
  const fmt = { b: "bold", i: "italic" }[event.key.toLowerCase()];
  if (fmt) {
    event.preventDefault();
    applyFormat(event.target, fmt);
  }
});

// ---------------------------------------------------------------------------
// @-Erwähnungen: Vorschläge beim Tippen von „@Na…“
// ---------------------------------------------------------------------------
let mentionTimer;
const MENTION_RE = /(^|\s)@([\p{L}\p{N}][\p{L}\p{N} .'-]{0,30})?$/u;
function mentionList(area) {
  return area.parentElement.querySelector(".mention-list");
}
function pickMention(area, name) {
  const list = mentionList(area);
  const at = Number(list.dataset.start);
  const end = area.selectionStart;
  area.value = area.value.slice(0, at) + "@" + name + " " + area.value.slice(end);
  const pos = at + name.length + 2;
  area.setSelectionRange(pos, pos);
  list.hidden = true;
  area.focus();
  notifyInput(area);
}
document.addEventListener("input", (event) => {
  const area = event.target;
  if (!area.matches("textarea[data-mentions]")) return;
  const list = mentionList(area);
  const upto = area.value.slice(0, area.selectionStart);
  const match = upto.match(MENTION_RE);
  clearTimeout(mentionTimer);
  if (!match) {
    list.hidden = true;
    return;
  }
  const term = match[2] || "";
  mentionTimer = setTimeout(async () => {
    const names = await fetch("/agents/mention?q=" + encodeURIComponent(term))
      .then((r) => (r.ok ? r.json() : []))
      .catch(() => []);
    if (!names.length) {
      list.hidden = true;
      return;
    }
    list.replaceChildren(
      ...names.map((name, i) => {
        const li = document.createElement("li");
        li.textContent = name;
        li.setAttribute("role", "option");
        li.dataset.name = name;
        if (i === 0) li.classList.add("active");
        return li;
      }),
    );
    list.dataset.start = upto.length - term.length - 1;
    list.hidden = false;
  }, 150);
});
document.addEventListener("mousedown", (event) => {
  const li = event.target.closest(".mention-list li");
  if (!li) return;
  event.preventDefault();
  pickMention(li.closest(".mention-wrap").querySelector("textarea"), li.dataset.name);
});
document.addEventListener(
  "keydown",
  (event) => {
    const area = event.target;
    if (!area.matches("textarea[data-mentions]")) return;
    const list = mentionList(area);
    if (list.hidden) return;
    const items = [...list.children];
    let idx = items.findIndex((li) => li.classList.contains("active"));
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      items[idx]?.classList.remove("active");
      idx = (idx + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
      items[idx].classList.add("active");
    } else if ((event.key === "Enter" || event.key === "Tab") && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      event.stopImmediatePropagation();
      pickMention(area, items[Math.max(idx, 0)].dataset.name);
    } else if (event.key === "Escape") {
      list.hidden = true;
    }
  },
  true,
);
document.addEventListener("focusout", (event) => {
  if (event.target.matches?.("textarea[data-mentions]")) {
    setTimeout(() => (mentionList(event.target).hidden = true), 150);
  }
});

// ---------------------------------------------------------------------------
// Tastenkürzel (außerhalb von Eingabefeldern); „?“ zeigt die Übersicht
// ---------------------------------------------------------------------------
const SHORTCUTS = [
  ["?", "Diese Übersicht"],
  ["g dann t", "Tickets / Ansichten"],
  ["g dann k", "Kunden"],
  ["g dann w", "Wissensdatenbank"],
  ["g dann b", "Benachrichtigungen"],
  ["n", "Neues Ticket"],
  ["a", "Anruf erfassen"],
  ["/", "Suche"],
  ["j / k", "Nächstes / vorheriges Ticket in der Liste"],
  ["o oder Enter", "Markiertes Ticket öffnen"],
  ["x", "Markiertes Ticket auswählen (Massenbearbeitung)"],
  ["r", "Im Ticket: öffentliche Antwort schreiben"],
  ["i", "Im Ticket: interne Notiz schreiben"],
  ["Strg + Enter", "Antwort absenden"],
  ["Strg + B / I", "Fett / kursiv"],
  ["Esc", "Eingabefeld verlassen"],
];

function showShortcuts() {
  let dlg = document.getElementById("cc-shortcuts");
  if (!dlg) {
    dlg = document.createElement("dialog");
    dlg.id = "cc-shortcuts";
    dlg.className = "confirm-dialog shortcuts-dialog";
    const rows = SHORTCUTS.map(([k, d]) => `<tr><td><kbd>${k}</kbd></td><td>${d}</td></tr>`).join("");
    dlg.innerHTML =
      `<h3>Tastenkürzel</h3><table class="shortcuts">${rows}</table>` +
      `<div class="confirm-actions"><button type="button" class="btn primary" data-close>Schließen</button></div>`;
    dlg.querySelector("[data-close]").onclick = () => dlg.close();
    document.body.append(dlg);
  }
  dlg.showModal();
}

function moveRow(step) {
  const rows = [...document.querySelectorAll("table.selectable tbody tr")];
  if (!rows.length) return false;
  let idx = rows.findIndex((r) => r.classList.contains("kb-focus"));
  rows[idx]?.classList.remove("kb-focus");
  idx = idx < 0 ? 0 : Math.min(Math.max(idx + step, 0), rows.length - 1);
  rows[idx].classList.add("kb-focus");
  rows[idx].scrollIntoView({ block: "nearest" });
  return true;
}
const focusedRow = () => document.querySelector("table.selectable tr.kb-focus");

// Navigation über einen echten Link, damit hx-boost und die Warnung bei
// ungespeicherten Änderungen greifen.
function go(url) {
  const link = document.createElement("a");
  link.href = url;
  document.body.append(link);
  if (window.htmx) htmx.process(link);
  link.click();
  link.remove();
}

let gPending = false;
document.addEventListener("keydown", (event) => {
  if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey) return;
  const t = event.target;
  if (t.matches("input, textarea, select, [contenteditable]")) {
    if (event.key === "Escape") t.blur();
    return;
  }
  if (document.querySelector("dialog[open]")) return;
  const key = event.key;
  if (gPending) {
    gPending = false;
    const dest = { t: "/views", k: "/users", w: "/kb", b: "/notifications" }[key];
    if (dest) {
      event.preventDefault();
      go(dest);
    }
    return;
  }
  let handled = true;
  switch (key) {
    case "g":
      gPending = true;
      setTimeout(() => (gPending = false), 1500);
      break;
    case "?":
      showShortcuts();
      break;
    case "n":
      go("/tickets/new?channel=web");
      break;
    case "a":
      go("/tickets/new?channel=phone");
      break;
    case "/":
      document.querySelector(".search input")?.focus();
      break;
    case "j":
    case "k":
      handled = moveRow(key === "j" ? 1 : -1);
      break;
    case "o":
    case "Enter": {
      const link = focusedRow()?.querySelector("a[href^='/tickets/']");
      handled = !!link;
      link?.click();
      break;
    }
    case "x": {
      const box = focusedRow()?.querySelector("input[type=checkbox]");
      handled = !!box;
      box?.click();
      break;
    }
    case "r":
    case "i": {
      const area = document.querySelector("textarea[name=comment]");
      handled = !!area;
      if (area) {
        const radio = document.querySelector(`.composer-tabs input[name=public][value="${key === "i" ? 0 : 1}"]`);
        if (radio && !radio.checked) radio.click();
        (area._editor || area).focus();
      }
      break;
    }
    default:
      handled = false;
  }
  if (handled) event.preventDefault();
});

// Ticketlisten aktualisieren sich von selbst, solange man nichts auswählt oder tippt.
window.ccListIdle = () =>
  !document.querySelector("input[name=ticket_id]:checked, tr.kb-focus") &&
  !document.activeElement?.matches("input, select, textarea") &&
  !document.querySelector("dialog[open]");

// ---------------------------------------------------------------------------
// Suchfeld in der Kopfzeile: Vorschläge per Tastatur bedienen, schließen
// ---------------------------------------------------------------------------
function quickBox() {
  return document.getElementById("quick-search");
}
function quickItems() {
  return [...(quickBox()?.querySelectorAll(".quick-item") || [])];
}
function closeQuick() {
  const box = quickBox();
  if (box) box.innerHTML = "";
}
document.addEventListener("keydown", (event) => {
  const input = event.target;
  const inField = input.matches?.(".search input[name=q]");
  const inBox = input.closest?.("#quick-search");
  if (!inField && !inBox) return;
  const items = quickItems();
  if (event.key === "Escape") {
    closeQuick();
    if (inBox) document.querySelector(".search input[name=q]")?.focus();
    return;
  }
  if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
  if (!items.length) return;
  event.preventDefault();
  const idx = items.indexOf(document.activeElement);
  const next = event.key === "ArrowDown" ? idx + 1 : idx - 1;
  if (next < 0) document.querySelector(".search input[name=q]")?.focus();
  else items[Math.min(next, items.length - 1)].focus();
});
document.addEventListener("focusout", () => {
  // Nach dem Fokuswechsel prüfen, ob er noch im Suchbereich ist.
  setTimeout(() => {
    if (!document.activeElement?.closest?.("form.search")) closeQuick();
  }, 150);
});

// ---------------------------------------------------------------------------
// Kurze Bestätigung nach dem Speichern (?ok=… in der Adresse)
// ---------------------------------------------------------------------------
function showToast(text) {
  let box = document.getElementById("cc-toast");
  if (!box) {
    box = document.createElement("div");
    box.id = "cc-toast";
    box.className = "toast";
    box.setAttribute("role", "status");
    document.body.append(box);
  }
  box.textContent = "✓ " + text;
  box.classList.add("show");
  clearTimeout(box._timer);
  box._timer = setTimeout(() => box.classList.remove("show"), 2800);
}
function consumeOk() {
  const url = new URL(location.href);
  const ok = url.searchParams.get("ok");
  if (!ok) return;
  url.searchParams.delete("ok");
  history.replaceState(history.state, "", url.pathname + (url.search || "") + url.hash);
  showToast(ok);
}
document.addEventListener("DOMContentLoaded", consumeOk);
document.addEventListener("htmx:afterSettle", consumeOk);

// ---------------------------------------------------------------------------
// Ungelesene Benachrichtigungen im Tab-Titel: „(3) Ticket …“
// ---------------------------------------------------------------------------
document.addEventListener("htmx:afterSwap", (event) => {
  if (!event.detail.target.matches?.(".bell")) return;
  const n = parseInt(event.detail.target.querySelector(".badge-count")?.textContent || "0", 10) || 0;
  const base = document.title.replace(/^\(\d+\+?\) /, "");
  document.title = n > 0 ? `(${n > 99 ? "99+" : n}) ${base}` : base;
});

// ---------------------------------------------------------------------------
// Hell/Dunkel: folgt dem System, lässt sich aber umschalten (pro Browser gespeichert)
// ---------------------------------------------------------------------------
const THEMES = ["auto", "dark", "light"];
const THEME_LABEL = { auto: "Design: wie System", dark: "Design: dunkel", light: "Design: hell" };
function currentTheme() {
  try {
    return localStorage.getItem("cc-theme") || "auto";
  } catch {
    return "auto";
  }
}
function applyTheme(theme) {
  if (theme === "auto") document.documentElement.removeAttribute("data-theme");
  else document.documentElement.dataset.theme = theme;
  document.querySelectorAll("[data-theme-toggle]").forEach((b) => {
    b.title = THEME_LABEL[theme];
    b.setAttribute("aria-label", THEME_LABEL[theme]);
  });
}
document.addEventListener("click", (event) => {
  if (!event.target.closest("[data-theme-toggle]")) return;
  const next = THEMES[(THEMES.indexOf(currentTheme()) + 1) % THEMES.length];
  try {
    localStorage.setItem("cc-theme", next);
  } catch {}
  applyTheme(next);
  showToast(THEME_LABEL[next]);
});
document.addEventListener("DOMContentLoaded", () => applyTheme(currentTheme()));
document.addEventListener("htmx:afterSettle", () => applyTheme(currentTheme()));
