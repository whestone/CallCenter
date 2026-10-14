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
  // Absenden des Ticketformulars selbst ist erlaubt.
  const leavesDirtyTicket = form && source !== form && source.form !== form;
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
  area.focus();
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
