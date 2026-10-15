// Editor mit sichtbarer Formatierung für Antworten (fett, kursiv, Listen, Links, Code).
//
// Aus jeder <textarea data-rte> wird ein bearbeitbares Feld; die Textarea bleibt
// versteckt im Formular und erhält bei jeder Änderung den Inhalt im schlichten
// Speicherformat (**fett**, *kursiv*, - Liste …). Der Server formatiert daraus
// sicheres HTML für die Anzeige und Klartext für E-Mails.
(() => {
  // -------------------------------------------------------------------------
  // Umwandlung Speicherformat → HTML (für vorhandenen Text, z. B. Entwürfe)
  // -------------------------------------------------------------------------
  function esc(s) {
    return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
  }
  function inlineToHtml(text) {
    // Maskierte Zeichen vorab sichern, damit sie nicht als Formatierung gelten.
    const keep = [];
    let s = text.replace(/\\([*`_[\\])/g, (_, c) => `\u0000${keep.push(c) - 1}\u0000`);
    s = esc(s)
      .replace(/`([^`]+)`/g, "<code>$1</code>")
      .replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>")
      .replace(/(^|[^\w*])\*(\S(?:.*?\S)?)\*(?!\w)/g, "$1<em>$2</em>")
      .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+|mailto:[^\s)]+)\)/g, '<a href="$2">$1</a>');
    return s.replace(/\u0000(\d+)\u0000/g, (_, i) => esc(keep[i]));
  }
  function mdToHtml(md) {
    const out = [];
    let list = null;
    let para = [];
    const flushPara = () => {
      if (para.length) out.push("<div>" + para.map(inlineToHtml).join("<br>") + "</div>");
      para = [];
    };
    const flushList = () => {
      if (list) out.push(`<${list.tag}>` + list.items.map((i) => `<li>${inlineToHtml(i)}</li>`).join("") + `</${list.tag}>`);
      list = null;
    };
    for (const line of md.replace(/\r\n/g, "\n").split("\n")) {
      const ul = line.match(/^\s*[-*•] (.*)$/);
      const ol = line.match(/^\s*\d{1,3}\. (.*)$/);
      if (ul || ol) {
        flushPara();
        const tag = ul ? "ul" : "ol";
        if (!list || list.tag !== tag) {
          flushList();
          list = { tag, items: [] };
        }
        list.items.push((ul || ol)[1]);
      } else if (!line.trim()) {
        flushPara();
        flushList();
        out.push("<div><br></div>");
      } else {
        flushList();
        para.push(line);
      }
    }
    flushPara();
    flushList();
    // Abschließende Leerzeile weglassen.
    while (out.length && out[out.length - 1] === "<div><br></div>") out.pop();
    return out.join("");
  }

  // -------------------------------------------------------------------------
  // Umwandlung HTML → Speicherformat
  // -------------------------------------------------------------------------
  const BLOCKS = new Set(["DIV", "P", "UL", "OL", "BLOCKQUOTE", "H1", "H2", "H3", "H4", "H5", "H6", "PRE", "TABLE", "TR"]);
  function wrap(marker, text) {
    // Leerzeichen gehören vor bzw. hinter die Markierung, sonst greift sie nicht.
    const m = text.match(/^(\s*)([\s\S]*?)(\s*)$/);
    return m[2] ? `${m[1]}${marker}${m[2]}${marker}${m[3]}` : text;
  }
  function children(node) {
    let out = "";
    for (const child of node.childNodes) {
      if (child.nodeType === 1 && BLOCKS.has(child.tagName) && out && !out.endsWith("\n")) out += "\n";
      out += toMd(child);
    }
    return out;
  }
  function toMd(n) {
    if (n.nodeType === 3) return n.nodeValue.replace(/ /g, " ").replace(/([*`\\])/g, "\\$1");
    if (n.nodeType !== 1) return "";
    switch (n.tagName) {
      case "BR":
        return "\n";
      case "B":
      case "STRONG":
        return wrap("**", children(n));
      case "I":
      case "EM":
        return wrap("*", children(n));
      case "CODE":
        return n.textContent ? "`" + n.textContent.replace(/`/g, "'") + "`" : "";
      case "A": {
        const href = n.getAttribute("href") || "";
        const text = children(n);
        if (!/^(https?:|mailto:)/i.test(href)) return text;
        return text === href || !text.trim() ? href : `[${text.replace(/[[\]]/g, "")}](${href})`;
      }
      case "UL":
      case "OL": {
        let i = 0;
        const items = [...n.children]
          .filter((c) => c.tagName === "LI")
          .map((li) => (n.tagName === "OL" ? `${++i}. ` : "- ") + children(li).replace(/\n+/g, " ").trim());
        return items.join("\n") + "\n";
      }
      case "BLOCKQUOTE":
        return children(n).trim().split("\n").map((l) => "> " + l).join("\n") + "\n";
      case "SCRIPT":
      case "STYLE":
      case "IMG":
        return "";
      default: {
        const text = children(n);
        return BLOCKS.has(n.tagName) && !text.endsWith("\n") ? text + "\n" : text;
      }
    }
  }
  function serialize(editor) {
    return children(editor)
      .replace(/[ \t]+\n/g, "\n")
      .replace(/\n{3,}/g, "\n\n")
      .trim();
  }

  // -------------------------------------------------------------------------
  // Editor einrichten
  // -------------------------------------------------------------------------
  function sync(editor) {
    const area = editor._area;
    const md = serialize(editor);
    if (area.value !== md) {
      area.value = md;
      area.dispatchEvent(new Event("input", { bubbles: true }));
    }
    editor.classList.toggle("is-empty", !editor.textContent.trim() && !editor.querySelector("li, img"));
  }

  function enhance(area) {
    if (area._editor) return;
    // Reste aus einem wiederhergestellten Seitenschnappschuss (Zurück-Taste) entfernen,
    // sonst stünden zwei Editoren untereinander.
    const stale = [...area.parentElement.querySelectorAll(":scope > .rte")];
    if (stale.some((e) => "mentions" in e.dataset)) area.setAttribute("data-mentions", "");
    stale.forEach((e) => e.remove());
    const editor = document.createElement("div");
    editor.className = "rte";
    editor.contentEditable = "true";
    editor.setAttribute("role", "textbox");
    editor.setAttribute("aria-multiline", "true");
    editor.setAttribute("aria-label", area.getAttribute("aria-label") || area.placeholder || "Text");
    editor.dataset.placeholder = area.placeholder || "";
    if (area.hasAttribute("data-mentions")) {
      editor.dataset.mentions = "";
      area.removeAttribute("data-mentions");
    }
    editor.innerHTML = mdToHtml(area.value);
    editor._area = area;
    area._editor = editor;
    area.hidden = true;
    area.before(editor);
    editor.addEventListener("input", () => sync(editor));
    // Eingefügter Text: Formatierung aus Word/Webseiten nur grob übernehmen.
    editor.addEventListener("paste", (event) => {
      const html = event.clipboardData?.getData("text/html");
      if (!html) return;
      event.preventDefault();
      const tmp = document.createElement("div");
      tmp.innerHTML = html;
      document.execCommand("insertHTML", false, mdToHtml(serialize(tmp)));
    });
    // Hilfe-Artikel & Co. fügen über die Textarea ein → in den Editor übernehmen.
    area.addEventListener("cc:replace", () => {
      editor.innerHTML = mdToHtml(area.value);
      sync(editor);
    });
    editor.classList.toggle("is-empty", !editor.textContent.trim());
  }

  function enhanceAll() {
    document.querySelectorAll("textarea[data-rte]").forEach(enhance);
  }
  document.addEventListener("DOMContentLoaded", enhanceAll);
  document.addEventListener("htmx:afterSettle", enhanceAll);
  document.addEventListener("htmx:historyRestore", enhanceAll);
  // Bevor htmx die Seite für „Zurück“ speichert: Editor wieder zur einfachen Textarea
  // machen (samt Entwurf), damit der Schnappschuss sauber ist.
  document.addEventListener("htmx:beforeHistorySave", () => {
    document.querySelectorAll(".rte").forEach((editor) => {
      const area = editor._area;
      if (!area) return editor.remove();
      sync(editor);
      area.textContent = area.value;
      if ("mentions" in editor.dataset) area.setAttribute("data-mentions", "");
      area.hidden = false;
      area._editor = null;
      editor.remove();
    });
  });
  if (document.readyState !== "loading") enhanceAll();

  // Vor dem Absenden sicherstellen, dass die Textarea den letzten Stand hat.
  document.addEventListener(
    "submit",
    (event) => event.target.querySelectorAll?.(".rte").forEach((ed) => sync(ed)),
    true,
  );

  // -------------------------------------------------------------------------
  // Formatierleiste
  // -------------------------------------------------------------------------
  function askLink(initial) {
    return new Promise((resolve) => {
      let dlg = document.getElementById("cc-link");
      if (!dlg) {
        dlg = document.createElement("dialog");
        dlg.id = "cc-link";
        dlg.className = "confirm-dialog";
        dlg.innerHTML =
          '<form method="dialog"><label>Link-Adresse<input type="url" name="url" placeholder="https://…" required></label>' +
          '<div class="confirm-actions"><button type="button" class="btn" value="cancel">Abbrechen</button>' +
          '<button class="btn primary" value="ok">Einfügen</button></div></form>';
        document.body.append(dlg);
        dlg.querySelector("button[value=cancel]").onclick = () => dlg.close("");
      }
      const input = dlg.querySelector("input");
      input.value = initial || "https://";
      dlg.onclose = () => resolve(dlg.returnValue === "ok" ? input.value.trim() : null);
      dlg.returnValue = "";
      dlg.showModal();
      input.select();
    });
  }

  // Knöpfe der Formatierleiste nehmen den Fokus nicht, sonst ginge die Markierung verloren.
  document.addEventListener("mousedown", (event) => {
    if (event.target.closest("[data-fmt]") && document.querySelector(".rte")) event.preventDefault();
  });

  window.ccEditorFormat = async (editor, fmt) => {
    editor.focus();
    const sel = window.getSelection();
    const range = sel.rangeCount ? sel.getRangeAt(0).cloneRange() : null;
    switch (fmt) {
      case "bold":
        document.execCommand("bold");
        break;
      case "italic":
        document.execCommand("italic");
        break;
      case "ul":
        document.execCommand("insertUnorderedList");
        break;
      case "ol":
        document.execCommand("insertOrderedList");
        break;
      case "code": {
        const text = sel.toString() || "Code";
        document.execCommand("insertHTML", false, `<code>${esc(text)}</code> `);
        break;
      }
      case "link": {
        const selected = sel.toString();
        const url = await askLink(/^https?:\/\//.test(selected) ? selected : "");
        editor.focus();
        if (range) {
          sel.removeAllRanges();
          sel.addRange(range);
        }
        if (!url || !/^(https?:\/\/|mailto:)/i.test(url)) break;
        if (selected) document.execCommand("createLink", false, url);
        else document.execCommand("insertHTML", false, `<a href="${esc(url)}">${esc(url)}</a> `);
        break;
      }
    }
    sync(editor);
  };

  // -------------------------------------------------------------------------
  // @-Erwähnungen im Editor
  // -------------------------------------------------------------------------
  const MENTION_RE = /(^|\s)@([\p{L}\p{N}][\p{L}\p{N} .'-]{0,30})?$/u;
  let timer;
  function listFor(editor) {
    return editor.parentElement.querySelector(".mention-list");
  }
  function caret(editor) {
    const sel = window.getSelection();
    if (!sel.rangeCount) return null;
    const r = sel.getRangeAt(0);
    if (!r.collapsed || r.startContainer.nodeType !== 3 || !editor.contains(r.startContainer)) return null;
    return { node: r.startContainer, offset: r.startOffset };
  }
  function pick(editor, name) {
    const list = listFor(editor);
    const node = list._node;
    if (!node || !editor.contains(node)) return;
    const start = list._start;
    const end = list._end;
    const text = node.nodeValue;
    node.nodeValue = text.slice(0, start) + "@" + name + " " + text.slice(end);
    const pos = start + name.length + 2;
    const range = document.createRange();
    range.setStart(node, Math.min(pos, node.nodeValue.length));
    range.collapse(true);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
    list.hidden = true;
    editor.focus();
    sync(editor);
  }
  document.addEventListener("input", (event) => {
    const editor = event.target.closest?.(".rte[data-mentions]");
    if (!editor) return;
    const list = listFor(editor);
    const c = caret(editor);
    const upto = c ? c.node.nodeValue.slice(0, c.offset) : "";
    const match = c && upto.replace(/ /g, " ").match(MENTION_RE);
    clearTimeout(timer);
    if (!match) {
      list.hidden = true;
      return;
    }
    const term = match[2] || "";
    timer = setTimeout(async () => {
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
      list._node = c.node;
      list._start = upto.length - term.length - 1;
      list._end = c.offset;
      list.hidden = false;
    }, 150);
  });
  document.addEventListener("mousedown", (event) => {
    const li = event.target.closest(".mention-list li");
    const editor = li?.closest(".mention-wrap")?.querySelector(".rte");
    if (!editor) return;
    event.preventDefault();
    pick(editor, li.dataset.name);
  });
  document.addEventListener(
    "keydown",
    (event) => {
      const editor = event.target.closest?.(".rte");
      if (!editor) return;
      // Strg+Enter sendet ab.
      if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
        event.preventDefault();
        sync(editor);
        editor._area.form?.requestSubmit();
        return;
      }
      const list = listFor(editor);
      if (!list || list.hidden) return;
      const items = [...list.children];
      let idx = items.findIndex((li) => li.classList.contains("active"));
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        items[idx]?.classList.remove("active");
        idx = (idx + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
        items[idx].classList.add("active");
      } else if (event.key === "Enter" || event.key === "Tab") {
        event.preventDefault();
        event.stopImmediatePropagation();
        pick(editor, items[Math.max(idx, 0)].dataset.name);
      } else if (event.key === "Escape") {
        list.hidden = true;
      }
    },
    true,
  );
  document.addEventListener("focusout", (event) => {
    const editor = event.target.closest?.(".rte");
    if (editor) setTimeout(() => listFor(editor) && (listFor(editor).hidden = true), 150);
  });
})();
