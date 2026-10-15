//! Einfache, sichere Textformatierung für Kommentare (Markdown-Teilmenge).
//!
//! Unterstützt **fett**, *kursiv*, `Code`, Listen (`- `, `1. `), Zitate (`> `),
//! Links `[Text](https://…)` und erkennt nackte URLs. Eingegebenes HTML wird
//! immer maskiert; Links sind nur für http(s) und mailto erlaubt.

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn safe_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:"))
        && !url.chars().any(|c| c.is_whitespace() || c == '"' || c == '<' || c == '>')
}

fn link(url: &str, text: &str) -> String {
    format!(r#"<a href="{}" target="_blank" rel="noopener noreferrer">{}</a>"#, escape(url), text)
}

fn find(chars: &[char], from: usize, pat: &[char]) -> Option<usize> {
    (from..chars.len().saturating_sub(pat.len() - 1)).find(|&i| chars[i..].starts_with(pat))
}

fn starts_with(chars: &[char], i: usize, pat: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    chars[i..].starts_with(&p)
}

/// Inline-Formatierung einer Zeile (Eingabe ist Rohtext, Ausgabe ist HTML).
pub fn inline(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let prev_alnum = i > 0 && chars[i - 1].is_alphanumeric();
        // \* \` \_ \[ \\ : Zeichen wörtlich (der Editor maskiert so getippte Sternchen usw.)
        if c == '\\' && chars.get(i + 1).is_some_and(|n| matches!(n, '*' | '`' | '_' | '[' | '\\')) {
            out.push_str(&escape(&chars[i + 1].to_string()));
            i += 2;
            continue;
        }
        // `Code`
        if c == '`' {
            if let Some(end) = find(&chars, i + 1, &['`']) {
                let inner: String = chars[i + 1..end].iter().collect();
                out.push_str(&format!("<code>{}</code>", escape(&inner)));
                i = end + 1;
                continue;
            }
        }
        // **fett**
        if starts_with(&chars, i, "**") {
            if let Some(end) = find(&chars, i + 2, &['*', '*']).filter(|e| *e > i + 2) {
                let inner: String = chars[i + 2..end].iter().collect();
                out.push_str(&format!("<strong>{}</strong>", inline(&inner)));
                i = end + 2;
                continue;
            }
        }
        // *kursiv* bzw. _kursiv_ (nicht mitten im Wort, z. B. snake_case)
        if (c == '*' || c == '_') && !prev_alnum && chars.get(i + 1).is_some_and(|n| !n.is_whitespace() && *n != c) {
            if let Some(end) = find(&chars, i + 1, &[c]) {
                let closes = !chars[end - 1].is_whitespace() && chars.get(end + 1).is_none_or(|n| !n.is_alphanumeric());
                if closes {
                    let inner: String = chars[i + 1..end].iter().collect();
                    out.push_str(&format!("<em>{}</em>", inline(&inner)));
                    i = end + 1;
                    continue;
                }
            }
        }
        // [Text](URL)
        if c == '[' {
            if let Some(mid) = find(&chars, i + 1, &[']', '(']) {
                if let Some(end) = find(&chars, mid + 2, &[')']) {
                    let label: String = chars[i + 1..mid].iter().collect();
                    let url: String = chars[mid + 2..end].iter().collect();
                    if safe_url(url.trim()) && !label.is_empty() {
                        out.push_str(&link(url.trim(), &escape(&label)));
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        // Nackte URL
        if !prev_alnum && (starts_with(&chars, i, "https://") || starts_with(&chars, i, "http://")) {
            let mut end = i;
            while end < chars.len() && !chars[end].is_whitespace() && !matches!(chars[end], '<' | '>' | '"') {
                end += 1;
            }
            // Satzzeichen am Ende gehören nicht zur URL.
            while end > i && matches!(chars[end - 1], '.' | ',' | ';' | ':' | '!' | '?' | ')') {
                end -= 1;
            }
            let url: String = chars[i..end].iter().collect();
            if url.len() > 8 {
                out.push_str(&link(&url, &escape(&url)));
                i = end;
                continue;
            }
        }
        out.push_str(&escape(&c.to_string()));
        i += 1;
    }
    out
}

enum Block<'a> {
    None,
    Para(Vec<&'a str>),
    List(&'static str, Vec<&'a str>),
    Quote(Vec<&'a str>),
}

fn flush(out: &mut String, block: &mut Block) {
    match std::mem::replace(block, Block::None) {
        Block::None => {}
        Block::Para(lines) => {
            out.push_str("<p>");
            out.push_str(&lines.iter().map(|l| inline(l)).collect::<Vec<_>>().join("<br>"));
            out.push_str("</p>");
        }
        Block::List(tag, items) => {
            out.push_str(&format!("<{tag}>"));
            for item in items {
                out.push_str(&format!("<li>{}</li>", inline(item)));
            }
            out.push_str(&format!("</{tag}>"));
        }
        Block::Quote(lines) => {
            out.push_str("<blockquote>");
            out.push_str(&render(&lines.join("\n")));
            out.push_str("</blockquote>");
        }
    }
}

fn list_item(line: &str) -> Option<(&'static str, &str)> {
    let t = line.trim_start();
    for marker in ["- ", "* ", "• "] {
        if let Some(rest) = t.strip_prefix(marker) {
            return Some(("ul", rest));
        }
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits <= 3 {
        if let Some(rest) = t[digits..].strip_prefix(". ") {
            return Some(("ol", rest));
        }
    }
    None
}

/// Kommentartext in HTML umwandeln.
pub fn render(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut out = String::new();
    let mut block = Block::None;
    for line in text.lines() {
        if line.trim().is_empty() {
            flush(&mut out, &mut block);
            continue;
        }
        if let Some((tag, item)) = list_item(line) {
            match &mut block {
                Block::List(t, items) if *t == tag => items.push(item),
                _ => {
                    flush(&mut out, &mut block);
                    block = Block::List(tag, vec![item]);
                }
            }
        } else if let Some(rest) = line.trim_start().strip_prefix('>') {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            match &mut block {
                Block::Quote(lines) => lines.push(rest),
                _ => {
                    flush(&mut out, &mut block);
                    block = Block::Quote(vec![rest]);
                }
            }
        } else {
            match &mut block {
                Block::Para(lines) => lines.push(line),
                _ => {
                    flush(&mut out, &mut block);
                    block = Block::Para(vec![line]);
                }
            }
        }
    }
    flush(&mut out, &mut block);
    out
}

/// Formatierten Kommentar als lesbaren Klartext (für E-Mails): ohne Sternchen,
/// Listen mit „- “, Links als „Text (URL)“.
pub fn to_plain(text: &str) -> String {
    let html = render(text)
        .replace("<br>", "\n")
        .replace("</p>", "\n\n")
        .replace("</li>", "\n")
        .replace("</ul>", "\n")
        .replace("</ol>", "\n");
    let mut out = String::new();
    let mut rest = html.as_str();
    let mut ol_counter: Option<u32> = None;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else { break };
        let tag = &rest[start + 1..start + end];
        let after = &rest[start + end + 1..];
        if tag == "ol" {
            ol_counter = Some(0);
        } else if tag == "ul" {
            ol_counter = None;
        } else if tag == "li" {
            match ol_counter.as_mut() {
                Some(n) => {
                    *n += 1;
                    out.push_str(&format!("{n}. "));
                }
                None => out.push_str("- "),
            }
        } else if tag == "blockquote" {
            out.push_str("> ");
        } else if let Some(href) = tag.strip_prefix("a href=\"").and_then(|t| t.split('"').next()) {
            // Linktext übernehmen, URL dahinter, falls sie nicht schon der Text ist.
            if let Some(close) = after.find("</a>") {
                let label = &after[..close];
                if label == href {
                    out.push_str(label);
                } else {
                    out.push_str(&format!("{label} ({href})"));
                }
                rest = &after[close + 4..];
                continue;
            }
        }
        rest = after;
    }
    out.push_str(rest);
    let out = out
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    // Mehrfache Leerzeilen zusammenfassen.
    let mut lines: Vec<&str> = Vec::new();
    for line in out.lines() {
        if line.trim().is_empty() && lines.last().is_none_or(|l| l.trim().is_empty()) {
            continue;
        }
        lines.push(line.trim_end());
    }
    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text() {
        assert_eq!(
            to_plain("Hallo **Max**,\n\n- eins\n- zwei\n\n1. a\n2. b\n\nSiehe [Hilfe](https://x.de/h) & https://y.de\n2 \\* 3 <b>"),
            "Hallo Max,\n\n- eins\n- zwei\n\n1. a\n2. b\n\nSiehe Hilfe (https://x.de/h) & https://y.de\n2 * 3 <b>"
        );
    }

    #[test]
    fn escapes_html() {
        assert_eq!(render("<script>alert(1)</script>"), "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>");
        assert_eq!(inline("[x](javascript:alert(1))"), "[x](javascript:alert(1))");
    }

    #[test]
    fn formatting() {
        assert_eq!(inline("**fett** und *kursiv* und `a<b`"), "<strong>fett</strong> und <em>kursiv</em> und <code>a&lt;b</code>");
        assert_eq!(inline("tag verpasster_anruf_x bleibt"), "tag verpasster_anruf_x bleibt");
        assert_eq!(inline("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(
            inline("siehe https://example.com/a?b=1."),
            r#"siehe <a href="https://example.com/a?b=1" target="_blank" rel="noopener noreferrer">https://example.com/a?b=1</a>."#
        );
        assert_eq!(
            inline("[Hilfe](https://x.de/h)"),
            r#"<a href="https://x.de/h" target="_blank" rel="noopener noreferrer">Hilfe</a>"#
        );
    }

    #[test]
    fn blocks() {
        assert_eq!(render("Hallo\nWelt\n\n- a\n- b\n\n1. x\n> zitat"), "<p>Hallo<br>Welt</p><ul><li>a</li><li>b</li></ul><ol><li>x</li></ol><blockquote><p>zitat</p></blockquote>");
    }
}
