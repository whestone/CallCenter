// Chip-Eingabe: <input data-chips="tags|emails"> zeigt Werte als Chips mit ×.
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

  function enhance(input) {
    if (input._chips) return;
    const kind = input.dataset.chips;
    // Reste aus einem wiederhergestellten Seitenschnappschuss entfernen.
    input.parentElement.querySelectorAll(":scope > .chips-input").forEach((e) => e.remove());

    const box = document.createElement("div");
    box.className = "chips-input";
    const entry = document.createElement("input");
    entry.type = "text";
    entry.className = "chips-entry";
    entry.autocomplete = "off";
    entry.placeholder = input.placeholder || "";
    entry.setAttribute("aria-label", input.getAttribute("aria-label") || (kind === "tags" ? "Tag hinzufügen" : "E-Mail hinzufügen"));
    if (kind === "tags") {
      const list = document.createElement("datalist");
      list.id = "chips-suggest-" + Math.random().toString(36).slice(2);
      entry.setAttribute("list", list.id);
      box.append(list);
      let timer;
      entry.addEventListener("input", () => {
        clearTimeout(timer);
        const q = entry.value.trim();
        if (!q) return;
        timer = setTimeout(async () => {
          const tags = await fetch("/tags/suggest?q=" + encodeURIComponent(q))
            .then((r) => (r.ok ? r.json() : []))
            .catch(() => []);
          list.replaceChildren(
            ...tags.filter((t) => !values().includes(t)).map((t) => Object.assign(document.createElement("option"), { value: t })),
          );
        }, 150);
      });
    }
    box.append(entry);
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
          entry.focus();
        });
        chip.append(x);
        box.insertBefore(chip, entry);
      }
      entry.placeholder = values().length ? "" : input.placeholder || "";
    }
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
      set(values().concat(parts));
      return true;
    }

    entry.addEventListener("keydown", (event) => {
      const separators = kind === "tags" ? ["Enter", " ", ",", ";"] : ["Enter", ",", ";", " "];
      if (separators.includes(event.key)) {
        if (entry.value.trim()) {
          event.preventDefault();
          commit();
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
    // Auswahl aus der Vorschlagsliste übernimmt den Tag sofort.
    entry.addEventListener("change", () => {
      if (kind === "tags" && entry.value.trim()) commit();
    });
    entry.addEventListener("blur", commit);
    entry.addEventListener("paste", () => setTimeout(commit, 0));
    box.addEventListener("click", (event) => {
      if (event.target === box) entry.focus();
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
