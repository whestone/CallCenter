// Grafischer Editor für Bedingungen, Aktionen, Sortierung und SLA-Zielzeiten.
// Der Editor schreibt sein Ergebnis als JSON in die zugehörige Textarea; das
// Backend bekommt also dasselbe Format wie über die JSON-Ansicht.
(() => {
  const metaEl = document.getElementById("rule-meta");
  if (!metaEl) return;
  const META = JSON.parse(metaEl.textContent);
  const condField = (key) => META.conditionFields.find((f) => f.key === key);
  const actField = (key) => META.actionFields.find((f) => f.key === key);

  const el = (tag, attrs = {}, ...children) => {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) {
      if (k === "class") node.className = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else if (v !== undefined && v !== null) node.setAttribute(k, v);
    }
    children.flat().forEach((c) => node.append(c));
    return node;
  };
  const select = (options, value, onchange, label) => {
    const s = el("select", { "aria-label": label, onchange: (e) => onchange(e.target.value) });
    options.forEach(([v, l]) => {
      const o = el("option", { value: String(v) }, l);
      if (String(v) === String(value ?? "")) o.selected = true;
      s.append(o);
    });
    return s;
  };

  // Werteingabe passend zum Feldtyp.
  const valueInput = (field, value, onchange) => {
    if (!field) return el("span");
    switch (field.type) {
      case "enum":
      case "id":
      case "bool": {
        const opts = field.options.length ? field.options : [["", "–"]];
        if (value === undefined || value === null) value = field.type === "id" ? "" : opts[0][0];
        return select(opts, value, (v) => onchange(field.type === "bool" ? v === "true" : v), "Wert");
      }
      case "longtext":
        return el("textarea", { rows: 3, placeholder: "Text … Platzhalter z. B. {{ticket.id}}", oninput: (e) => onchange(e.target.value) }, value ?? "");
      case "number":
        return el("input", { type: "number", step: "1", value: value ?? "", placeholder: "Stunden", oninput: (e) => onchange(e.target.value) });
      case "tags":
        return el("input", { type: "text", value: Array.isArray(value) ? value.join(" ") : value ?? "", placeholder: "tag1 tag2", oninput: (e) => onchange(e.target.value) });
      default:
        return el("input", { type: "text", value: value ?? "", oninput: (e) => onchange(e.target.value) });
    }
  };

  // Zahlen-IDs als Zahl speichern, Rest unverändert.
  const normalize = (field, v) => (field && field.type === "id" && /^\d+$/.test(String(v)) ? Number(v) : v === "" && field && field.type === "id" ? null : v);

  // ----- Bedingungen -----
  function conditionsBuilder(root, data, save) {
    data = data && typeof data === "object" ? data : {};
    data.all = Array.isArray(data.all) ? data.all : [];
    data.any = Array.isArray(data.any) ? data.any : [];
    const render = () => {
      root.replaceChildren(
        group("Alle diese Bedingungen müssen zutreffen", data.all),
        group("… und mindestens eine dieser (optional)", data.any),
      );
      save(data);
    };
    const group = (title, list) =>
      el("div", { class: "rb-group" },
        el("div", { class: "rb-group-title" }, title),
        list.map((c, i) => conditionRow(list, c, i)),
        el("button", { type: "button", class: "btn rb-add", onclick: () => { list.push({ field: "status", operator: "is", value: "open" }); render(); } }, "+ Bedingung"),
      );
    const conditionRow = (list, c, i) => {
      const field = condField(c.field) || META.conditionFields[0];
      if (!field.ops.includes(c.operator)) c.operator = field.ops[0];
      const needsValue = !META.noValueOps.includes(c.operator);
      return el("div", { class: "rb-row rb-cond" },
        select(META.conditionFields.map((f) => [f.key, f.label]), field.key, (v) => {
          const f = condField(v); list[i] = { field: v, operator: f.ops[0], value: f.options[0] ? normalize(f, f.options[0][0]) : "" }; render();
        }, "Feld"),
        select(field.ops.map((o) => [o, META.operatorLabels[o] || o]), c.operator, (v) => { c.operator = v; render(); }, "Operator"),
        needsValue ? valueInput(field, c.value, (v) => { c.value = normalize(field, v); save(data); }) : el("span", { class: "muted small rb-novalue" }, "—"),
        el("button", { type: "button", class: "link danger", title: "Entfernen", onclick: () => { list.splice(i, 1); render(); } }, "✕"),
      );
    };
    render();
  }

  // ----- Aktionen -----
  function actionsBuilder(root, data, save) {
    const list = Array.isArray(data) ? data : [];
    const render = () => {
      root.replaceChildren(
        el("div", { class: "rb-group" },
          list.map((a, i) => {
            const field = actField(a.field) || META.actionFields[0];
            return el("div", { class: "rb-row rb-act" },
              select(META.actionFields.map((f) => [f.key, f.label]), field.key, (v) => {
                const f = actField(v); list[i] = { field: v, value: f.options[0] ? normalize(f, f.options[0][0]) : "" }; render();
              }, "Aktion"),
              valueInput(field, a.value, (v) => { a.value = normalize(field, v); save(list); }),
              el("button", { type: "button", class: "link danger", title: "Entfernen", onclick: () => { list.splice(i, 1); render(); } }, "✕"),
            );
          }),
          el("button", { type: "button", class: "btn rb-add", onclick: () => { list.push({ field: "status", value: "open" }); render(); } }, "+ Aktion"),
        ),
      );
      save(list);
    };
    render();
  }

  // ----- Sortierung (Ansichten) -----
  function executionBuilder(root, data, save) {
    data = data && typeof data === "object" ? data : {};
    data.sort_by = data.sort_by || "updated_at";
    data.sort_order = data.sort_order || "desc";
    root.replaceChildren(
      el("div", { class: "rb-row rb-exec" },
        el("span", {}, "Sortieren nach"),
        select(META.sortFields, data.sort_by, (v) => { data.sort_by = v; save(data); }, "Sortierfeld"),
        select([["desc", "absteigend"], ["asc", "aufsteigend"]], data.sort_order, (v) => { data.sort_order = v; save(data); }, "Reihenfolge"),
      ),
    );
    save(data);
  }

  // ----- SLA-Zielzeiten: Tabelle Priorität × Metrik -----
  function metricsBuilder(root, data, save) {
    const list = Array.isArray(data) ? data : [];
    let business = list.some((x) => x.business_hours);
    const get = (p, m) => list.find((x) => x.priority === p && x.metric === m);
    const write = () =>
      save(list.filter((x) => Number(x.target) > 0).map((x) => ({ ...x, target: Number(x.target), business_hours: business })));
    const businessToggle = el("label", { class: "check rb-business" },
      el("input", { type: "checkbox", checked: business ? "" : undefined, onchange: (e) => { business = e.target.checked; write(); } }),
      " In Geschäftszeiten rechnen (Nächte, Wochenenden und Feiertage zählen nicht)",
    );
    const toHours = (min) => (min ? String(+(min / 60).toFixed(2)).replace(".", ",") : "");
    const table = el("table", { class: "tickets compact rb-sla" },
      el("thead", {}, el("tr", {}, el("th", {}, "Priorität"), META.metrics.map(([, l]) => el("th", {}, l + " (Stunden)")))),
      el("tbody", {}, META.priorities.map(([p, pl]) =>
        el("tr", {}, el("td", {}, pl), META.metrics.map(([m]) =>
          el("td", {}, el("input", {
            type: "text", inputmode: "decimal", value: toHours(get(p, m)?.target), placeholder: "–", "aria-label": `${pl} ${m}`,
            oninput: (e) => {
              const hours = parseFloat(e.target.value.replace(",", "."));
              let entry = get(p, m);
              if (!entry) { entry = { priority: p, metric: m, target: 0 }; list.push(entry); }
              entry.target = Number.isFinite(hours) && hours > 0 ? Math.round(hours * 60) : 0;
              write();
            },
          })),
        )),
      )),
    );
    root.replaceChildren(
      businessToggle,
      table,
      el("p", { class: "muted small" }, "Leer lassen = kein Ziel für diese Kombination. Dezimalwerte erlaubt, z. B. 0,5 = 30 Minuten. Geschäftszeiten und Feiertage: Verwaltung → Geschäftszeiten."),
    );
    write();
  }

  const BUILDERS = { conditions: conditionsBuilder, actions: actionsBuilder, execution: executionBuilder, metrics: metricsBuilder };

  document.querySelectorAll("[data-builder]").forEach((root) => {
    const build = BUILDERS[root.dataset.builder];
    const textarea = document.getElementById(root.dataset.target);
    if (!build || !textarea) return;
    let writing = false;
    const save = (value) => {
      writing = true;
      textarea.value = JSON.stringify(value, null, 2);
      writing = false;
    };
    const start = () => {
      let data;
      try { data = JSON.parse(textarea.value || "null"); }
      catch { root.replaceChildren(el("div", { class: "alert" }, "Das JSON ist ungültig – bitte in der JSON-Ansicht korrigieren.")); return; }
      build(root, data, save);
    };
    // Änderungen in der JSON-Ansicht übernimmt der grafische Editor beim Verlassen des Feldes.
    textarea.addEventListener("change", () => { if (!writing) start(); });
    start();
  });
})();
