// Chip-Eingabe: <input data-chips="tags|emails"> zeigt Werte als Chips mit × und
// dahinter einen „+“-Knopf; erst ein Klick darauf öffnet das Eingabefeld.
// Vorschläge: Tags aus bestehenden Tickets, bei E-Mails bestehende Kunden (Name + Adresse).
// Das ursprüngliche Feld bleibt (versteckt) im Formular und enthält die Werte als Text,
// damit Server, „ungespeichert“-Markierung und Absenden unverändert funktionieren.
(() => {
  const SPLIT = /[\s,;]+/;

  function normalize(kind, raw) {
    const v = raw.trim().replace(/^[,;]+|[,;]+$/g, "");
    if (!v) return "";
    return kind === "tags" ? v.toLowerCase().replace(/\s+/g, "_") : v;
  }
  function valid(kind, v) {
    return kind === "tags" ? true : /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(v);
  }

  async function suggestions(kind, q) {
    const url = (kind === "tags" ? "/tags/suggest?q=" : "/emails/suggest?q=") + encodeURIComponent(q);
    const data = await fetch(url).then((r) => (r.ok ? r.json() : [])).catch(() => []);
    return kind === "tags" ? data.map((t) => ({ value: t, label: t })) : data.map((u) => ({ value: u.email, label: u.name, detail: u.email }));
  }

  function enhance(input) {
    if (input._chips) return;
    const kind = input.dataset.chips;
    const noun = kind === "tags" ? "Tag" : "E-Mail-Adresse";
    // Reste aus einem wiederhergestellten Seitenschnappschuss entfernen.
    input.parentElement.querySelectorAll(":scope > .chips-input").forEach((e) => e.remove());

    const box = document.createElement("div");
    box.className = "chips-input";
    const add = document.createElement("button");
    add.type = "button";
    add.className = "chips-add";
    add.title = `${noun} hinzufügen`;
    add.setAttribute("aria-label", `${noun} hinzufügen`);
    add.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 5v14M5 12h14"/></svg>';
    const entry = document.createElement("input");
    entry.type = "text";
    entry.className = "chips-entry";
    entry.autocomplete = "off";
    entry.hidden = true;
    entry.placeholder = kind === "tags" ? "Tag eingeben …" : "Name oder E-Mail …";
    entry.setAttribute("aria-label", input.getAttribute("aria-label") || `${noun} hinzufügen`);
    entry.setAttribute("role", "combobox");
    entry.setAttribute("aria-autocomplete", "list");
    entry.setAttribute("aria-expanded", "false");
    const list = document.createElement("ul");
    list.className = "chips-suggest";
    list.setAttribute("role", "listbox");
    list.hidden = true;
    box.append(add, entry, list);
    input.hidden = true;
    input.after(box);
    input._chips = box;

    const values = () => input.value.split(SPLIT).map((v) => normalize(kind, v)).filter(Boolean);
    function set(vals) {
      const unique = [...new Set(vals)];
      input.value = unique.join(kind === "emails" ? ", " : " ");
      input.dispatchEvent(new Event("input", { bubbles: true }));
      render();
    }
    function render() {
      box.querySelectorAll(".chip-item").forEach((c) => c.remove());
      for (const v of values()) {
        const chip = document.createElement("span");
        chip.className = "chip-item" + (kind === "tags" ? " chip-tag" : " chip-mail");
        chip.textContent = v;
        const x = document.createElement("button");
        x.type = "button";
        x.className = "chip-x";
        x.textContent = "×";
        x.setAttribute("aria-label", `${v} entfernen`);
        x.addEventListener("click", () => {
          set(values().filter((o) => o !== v));
          add.focus();
        });
        chip.append(x);
        box.insertBefore(chip, add);
      }
    }

    // --- Eingabe öffnen/schließen
    function open() {
      add.hidden = true;
      entry.hidden = false;
      entry.focus();
    }
    function close() {
      hideList();
      entry.value = "";
      entry.hidden = true;
      add.hidden = false;
      box.classList.remove("chips-invalid");
      entry.title = "";
    }
    add.addEventListener("click", open);

    // --- Vorschläge
    let items = [];
    let active = -1;
    let timer;
    function hideList() {
      list.hidden = true;
      entry.setAttribute("aria-expanded", "false");
      items = [];
      active = -1;
    }
    function paint() {
      list.querySelectorAll("li").forEach((li, i) => li.classList.toggle("active", i === active));
    }
    function showList(found) {
      const taken = values();
      items = found.filter((f) => !taken.includes(normalize(kind, f.value)));
      active = -1;
      list.replaceChildren(...items.map((it, i) => {
        const li = document.createElement("li");
        li.setAttribute("role", "option");
        const main = document.createElement("span");
        main.textContent = it.label;
        li.append(main);
        if (it.detail) {
          const d = document.createElement("small");
          d.textContent = it.detail;
          li.append(d);
        }
        li.addEventListener("mousedown", (e) => {
          e.preventDefault(); // Fokus im Eingabefeld behalten
          pick(i);
        });
        return li;
      }));
      list.hidden = items.length === 0;
      entry.setAttribute("aria-expanded", String(!list.hidden));
    }
    function pick(i) {
      const it = items[i];
      if (!it) return;
      entry.value = "";
      hideList();
      box.classList.remove("chips-invalid");
      set(values().concat(normalize(kind, it.value)));
      entry.focus();
    }
    entry.addEventListener("input", () => {
      clearTimeout(timer);
      const q = entry.value.trim();
      if (!q) return hideList();
      timer = setTimeout(async () => {
        const found = await suggestions(kind, q);
        if (entry.value.trim() === q) showList(found);
      }, 150);
    });

    function commit() {
      const parts = entry.value.split(SPLIT).map((v) => normalize(kind, v)).filter(Boolean);
      if (!parts.length) {
        entry.value = "";
        return true;
      }
      const bad = parts.filter((p) => !valid(kind, p));
      if (bad.length) {
        entry.value = bad.join(" ");
        box.classList.add("chips-invalid");
        entry.title = "Keine gültige E-Mail-Adresse";
        return false;
      }
      box.classList.remove("chips-invalid");
      entry.title = "";
      entry.value = "";
      hideList();
      set(values().concat(parts));
      return true;
    }

    entry.addEventListener("keydown", (event) => {
      if (!list.hidden && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
        event.preventDefault();
        const n = items.length;
        active = event.key === "ArrowDown" ? (active + 1) % n : (active - 1 + n) % n;
        paint();
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        if (!list.hidden) hideList();
        else {
          close();
          add.focus();
        }
        return;
      }
      // Bei E-Mails darf im Suchtext ein Leerzeichen stehen (Vor- und Nachname).
      const separators = kind === "tags" ? ["Enter", " ", ",", ";"] : ["Enter", ",", ";"];
      if (separators.includes(event.key)) {
        if (event.key === "Enter" && active >= 0) {
          event.preventDefault();
          pick(active);
        } else if (entry.value.trim()) {
          event.preventDefault();
          // Eindeutiger Treffer bei Namenssuche (E-Mail): diesen übernehmen.
          if (kind === "emails" && !valid(kind, entry.value.trim()) && items.length === 1) pick(0);
          else commit();
        } else if (event.key === "Enter") {
          event.preventDefault(); // Enter im leeren Chip-Feld schickt nicht das Ticket ab
        }
      } else if (event.key === "Backspace" && !entry.value) {
        const vals = values();
        if (vals.length) {
          event.preventDefault();
          set(vals.slice(0, -1));
        }
      }
    });
    entry.addEventListener("blur", () => {
      setTimeout(() => {
        if (document.activeElement === entry) return;
        if (commit()) close();
      }, 0);
    });
    entry.addEventListener("paste", () => setTimeout(commit, 0));
    box.addEventListener("click", (event) => {
      if (event.target === box) open();
    });
    // Vor dem Absenden noch nicht bestätigten Text übernehmen.
    input.form?.addEventListener("submit", commit, true);
    render();
  }

  function enhanceAll() {
    document.querySelectorAll("input[data-chips]").forEach(enhance);
  }
  document.addEventListener("DOMContentLoaded", enhanceAll);
  document.addEventListener("htmx:afterSettle", enhanceAll);
  document.addEventListener("htmx:historyRestore", enhanceAll);
  if (document.readyState !== "loading") enhanceAll();

  // Vor dem Schnappschuss für „Zurück“ die Chips entfernen (Feld enthält den Wert).
  document.addEventListener("htmx:beforeHistorySave", () => {
    document.querySelectorAll("input[data-chips]").forEach((input) => {
      input.setAttribute("value", input.value);
      input.hidden = false;
      input._chips?.remove();
      input._chips = null;
    });
  });
})();
