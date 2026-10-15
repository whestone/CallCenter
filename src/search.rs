//! Ticketsuche mit Zendesk-ähnlicher Syntax.
//!
//! - Wörter: `drucker toner` (alle müssen vorkommen – in Betreff, Beschreibung,
//!   Kommentaren oder beim Anfragenden), Phrasen in Anführungszeichen: `"kein internet"`
//! - Filter: `status:open|unsolved` `priority:` `type:` `channel:` `tag:` `assignee:me|none|…`
//!   `requester:` `group:` `organization:` – Werte mit Leerzeichen in Anführungszeichen
//! - Datum: `created>2026-10-01` `updated<7d` `solved:heute` (Operatoren `: > < >= <=`,
//!   Werte JJJJ-MM-TT, TT.MM.JJJJ, `Nd` = vor N Tagen, `heute`, `gestern`)
//! - Ausschließen mit `-`: `-tag:spam` `-status:closed` `-drucker`
//! - `#123` bzw. `123` findet auch das Ticket mit dieser Nummer

use chrono::{Duration, Local, NaiveDate};
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::{
    error::AppResult,
    markdown::escape,
    models::{Ticket, User},
    service::normalize_phone,
};

/// Höchstzahl der Treffer; darüber wird zum Eingrenzen geraten.
pub const LIMIT: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub negate: bool,
    pub key: String,
    pub op: String,
    pub value: String,
}

/// Zerlegt die Suchanfrage; berücksichtigt Anführungszeichen und `-`.
pub fn tokenize(query: &str) -> Vec<Token> {
    let mut raw: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in query.chars() {
        match c {
            '"' | '„' | '“' | '”' => {
                quoted = !quoted;
                cur.push('"');
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    raw.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        raw.push(cur);
    }
    raw.into_iter()
        .filter_map(|t| {
            let (negate, t) = match t.strip_prefix('-') {
                Some(rest) if !rest.is_empty() => (true, rest.to_string()),
                _ => (false, t),
            };
            // Schlüssel und Operator nur außerhalb von Anführungszeichen erkennen.
            let split = if t.starts_with('"') {
                None
            } else {
                t.find([':', '>', '<']).map(|i| {
                    let op_len = if t[i + 1..].starts_with('=') { 2 } else { 1 };
                    (t[..i].to_lowercase(), t[i..i + op_len].to_string(), t[i + op_len..].to_string())
                })
            };
            let (key, op, value) = match split {
                Some((k, op, v)) if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic()) => (k, op, v),
                _ => (String::new(), String::new(), t),
            };
            let value = value.trim_matches('"').trim().to_string();
            // Reine Satzzeichen (z. B. ein einzelnes „-“) sind kein Suchbegriff.
            value.chars().any(char::is_alphanumeric).then_some(Token { negate, key, op, value })
        })
        .collect()
}

/// Freitext-Begriffe (für Hervorhebung und Relevanz).
pub fn text_terms(tokens: &[Token]) -> Vec<String> {
    tokens
        .iter()
        .filter(|t| t.key.is_empty() && !t.negate)
        .map(|t| t.value.trim_start_matches('#').to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

fn like(s: &str) -> String {
    format!("%{}%", s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

/// Datumswert in einen Tag umwandeln.
pub fn parse_day(v: &str) -> Option<NaiveDate> {
    let today = Local::now().date_naive();
    let v = v.trim().to_lowercase();
    match v.as_str() {
        "heute" | "today" => return Some(today),
        "gestern" | "yesterday" => return Some(today - Duration::days(1)),
        _ => {}
    }
    if let Some(n) = v.strip_suffix('d').or_else(|| v.strip_suffix('t')).and_then(|n| n.parse::<i64>().ok()) {
        return Some(today - Duration::days(n));
    }
    NaiveDate::parse_from_str(&v, "%Y-%m-%d").or_else(|_| NaiveDate::parse_from_str(&v, "%d.%m.%Y")).ok()
}

/// Fehler in der Anfrage, die dem Nutzer angezeigt werden.
pub fn problems(tokens: &[Token]) -> Vec<String> {
    tokens
        .iter()
        .filter(|t| matches!(t.key.as_str(), "created" | "updated" | "solved") && parse_day(&t.value).is_none())
        .map(|t| format!("Datum „{}“ nicht erkannt (z. B. 2026-10-01, 7d, heute)", t.value))
        .collect()
}

fn push_condition(qb: &mut QueryBuilder<'_, Postgres>, t: &Token, me: &User) {
    let v = t.value.to_lowercase();
    match t.key.as_str() {
        "status" if v == "unsolved" || v == "offen" => {
            qb.push("t.status IN ('new', 'open', 'pending', 'hold')");
        }
        "status" => {
            qb.push("t.status = ").push_bind(v);
        }
        "priority" => {
            qb.push("t.priority = ").push_bind(v);
        }
        "type" => {
            qb.push("t.ticket_type = ").push_bind(v);
        }
        "channel" | "via" => {
            qb.push("t.channel = ").push_bind(v);
        }
        "tag" | "tags" => {
            qb.push_bind(v).push(" = ANY(t.tags)");
        }
        "assignee" if v == "me" || v == "ich" => {
            qb.push("t.assignee_id = ").push_bind(me.id);
        }
        "assignee" if v == "none" || v == "niemand" => {
            qb.push("t.assignee_id IS NULL");
        }
        "assignee" | "requester" => {
            let col = if t.key == "assignee" { "t.assignee_id" } else { "t.requester_id" };
            qb.push(format!("{col} IN (SELECT id FROM users WHERE lower(email) = "))
                .push_bind(v.clone())
                .push(" OR phone = ")
                .push_bind(normalize_phone(&t.value))
                .push(" OR name ILIKE ")
                .push_bind(like(&t.value))
                .push(")");
        }
        "group" => {
            qb.push("t.group_id IN (SELECT id FROM agent_groups WHERE name ILIKE ").push_bind(like(&t.value)).push(")");
        }
        "organization" | "org" => {
            qb.push("t.organization_id IN (SELECT id FROM organizations WHERE name ILIKE ")
                .push_bind(like(&t.value))
                .push(")");
        }
        "created" | "updated" | "solved" => {
            let col = match t.key.as_str() {
                "created" => "t.created_at",
                "updated" => "t.updated_at",
                _ => "t.solved_at",
            };
            // Unbekannte Daten wurden vorher gemeldet; hier neutral behandeln.
            let Some(day) = parse_day(&t.value) else {
                qb.push("TRUE");
                return;
            };
            let next = day + Duration::days(1);
            match t.op.as_str() {
                ">" => qb.push(format!("{col} >= ")).push_bind(next).push("::date"),
                ">=" => qb.push(format!("{col} >= ")).push_bind(day).push("::date"),
                "<" => qb.push(format!("{col} < ")).push_bind(day).push("::date"),
                "<=" => qb.push(format!("{col} < ")).push_bind(next).push("::date"),
                _ => qb
                    .push(format!("({col} >= "))
                    .push_bind(day)
                    .push(format!("::date AND {col} < "))
                    .push_bind(next)
                    .push("::date)"),
            };
        }
        _ => {
            // Freitext (bei unbekanntem Schlüssel den ganzen Ausdruck als Text suchen).
            let term = if t.key.is_empty() { t.value.clone() } else { format!("{}{}{}", t.key, t.op, t.value) };
            let term = term.trim_start_matches('#').to_string();
            let p = like(&term);
            qb.push("(t.subject ILIKE ")
                .push_bind(p.clone())
                .push(" OR t.description ILIKE ")
                .push_bind(p.clone())
                .push(" OR EXISTS (SELECT 1 FROM ticket_comments c WHERE c.ticket_id = t.id AND c.body ILIKE ")
                .push_bind(p.clone())
                .push(") OR t.requester_id IN (SELECT id FROM users WHERE name ILIKE ")
                .push_bind(p.clone())
                .push(" OR email ILIKE ")
                .push_bind(p)
                .push(")");
            if let Ok(id) = term.parse::<i64>() {
                qb.push(" OR t.id = ").push_bind(id);
            }
            qb.push(")");
        }
    }
}

pub const SORTS: [(&str, &str); 4] =
    [("relevance", "Relevanz"), ("updated", "Zuletzt aktualisiert"), ("created", "Neueste zuerst"), ("oldest", "Älteste zuerst")];

/// Sucht Tickets; liefert höchstens `LIMIT + 1` Treffer (der letzte zeigt „es gibt mehr“).
pub async fn tickets(db: &PgPool, query: &str, me: &User, sort: &str) -> AppResult<Vec<Ticket>> {
    let tokens = tokenize(query);
    let terms = text_terms(&tokens);
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("SELECT t.* FROM tickets t WHERE TRUE");
    for t in &tokens {
        qb.push(if t.negate { " AND NOT (" } else { " AND (" });
        push_condition(&mut qb, t, me);
        qb.push(")");
    }
    match sort {
        "created" => {
            qb.push(" ORDER BY t.created_at DESC");
        }
        "oldest" => {
            qb.push(" ORDER BY t.created_at ASC");
        }
        "relevance" if !terms.is_empty() => {
            // Treffer im Betreff zählen am meisten, exakte Ticketnummer gewinnt immer.
            qb.push(" ORDER BY (0");
            for term in &terms {
                let p = like(term);
                qb.push(" + CASE WHEN t.subject ILIKE ").push_bind(p.clone()).push(" THEN 3 ELSE 0 END");
                qb.push(" + CASE WHEN t.description ILIKE ").push_bind(p).push(" THEN 1 ELSE 0 END");
                if let Ok(id) = term.parse::<i64>() {
                    qb.push(" + CASE WHEN t.id = ").push_bind(id).push(" THEN 100 ELSE 0 END");
                }
            }
            qb.push(") DESC, t.updated_at DESC");
        }
        _ => {
            qb.push(" ORDER BY t.updated_at DESC");
        }
    }
    qb.push(format!(" LIMIT {}", LIMIT + 1));
    Ok(qb.build_query_as::<Ticket>().fetch_all(db).await?)
}

/// Textausschnitt um den ersten Treffer, HTML-maskiert, Treffer mit `<mark>`.
pub fn excerpt(text: &str, terms: &[String]) -> Option<String> {
    let lower = text.to_lowercase();
    let (pos, len) = terms
        .iter()
        .filter_map(|t| lower.find(&t.to_lowercase()).map(|p| (p, t.len())))
        .min_by_key(|(p, _)| *p)?;
    // Auf Zeichengrenzen runden (Umlaute!).
    let mut start = pos.saturating_sub(60);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (pos + len + 100).min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    let slice: String = text[start..end].split_whitespace().collect::<Vec<_>>().join(" ");
    Some(format!(
        "{}{}{}",
        if start > 0 { "… " } else { "" },
        highlight(&slice, terms),
        if end < text.len() { " …" } else { "" }
    ))
}

/// HTML-maskierter Text mit hervorgehobenen Suchbegriffen.
pub fn highlight(text: &str, terms: &[String]) -> String {
    let lower = text.to_lowercase();
    // Nur wenn Kleinschreibung die Länge nicht ändert, sind Positionen übertragbar.
    if lower.len() != text.len() {
        return escape(text);
    }
    let mut marks: Vec<(usize, usize)> = Vec::new();
    for term in terms.iter().map(|t| t.to_lowercase()).filter(|t| !t.is_empty()) {
        let mut from = 0;
        while let Some(i) = lower[from..].find(&term) {
            marks.push((from + i, from + i + term.len()));
            from += i + term.len();
        }
    }
    marks.sort();
    let mut out = String::new();
    let mut at = 0;
    for (s, e) in marks {
        if s < at {
            continue;
        }
        out.push_str(&escape(&text[at..s]));
        out.push_str("<mark>");
        out.push_str(&escape(&text[s..e]));
        out.push_str("</mark>");
        at = e;
    }
    out.push_str(&escape(&text[at..]));
    out
}

/// Ausschnitte für Suchtreffer: aus Beschreibung oder dem neuesten passenden Kommentar.
pub async fn snippets(db: &PgPool, tickets: &[Ticket], terms: &[String]) -> AppResult<std::collections::HashMap<i64, String>> {
    let mut out = std::collections::HashMap::new();
    if terms.is_empty() || tickets.is_empty() {
        return Ok(out);
    }
    let ids: Vec<i64> = tickets.iter().map(|t| t.id).collect();
    let patterns: Vec<String> = terms.iter().map(|t| like(t)).collect();
    let comments: Vec<(i64, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (ticket_id) ticket_id, body FROM ticket_comments
         WHERE ticket_id = ANY($1) AND body ILIKE ANY($2) ORDER BY ticket_id, id DESC",
    )
    .bind(&ids)
    .bind(&patterns)
    .fetch_all(db)
    .await?;
    let comments: std::collections::HashMap<i64, String> = comments.into_iter().collect();
    for t in tickets {
        let snippet = excerpt(&t.description, terms).or_else(|| comments.get(&t.id).and_then(|c| excerpt(c, terms)));
        if let Some(s) = snippet {
            out.insert(t.id, s);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(negate: bool, key: &str, op: &str, value: &str) -> Token {
        Token { negate, key: key.into(), op: op.into(), value: value.into() }
    }

    #[test]
    fn tokens() {
        assert_eq!(
            tokenize(r#"drucker "kein internet" -tag:spam requester:"Max Mustermann" created>=7d #12"#),
            vec![
                tok(false, "", "", "drucker"),
                tok(false, "", "", "kein internet"),
                tok(true, "tag", ":", "spam"),
                tok(false, "requester", ":", "Max Mustermann"),
                tok(false, "created", ">=", "7d"),
                tok(false, "", "", "#12"),
            ]
        );
        // Uhrzeiten oder URLs sind kein Filter.
        assert_eq!(tokenize("https://x.de")[0].key, "https");
        assert_eq!(tokenize("10:30")[0].key, "");
        assert!(tokenize(r#"- """#).is_empty());
    }

    #[test]
    fn days_and_highlight() {
        assert!(parse_day("2026-10-01").is_some());
        assert!(parse_day("01.10.2026").is_some());
        assert!(parse_day("7d").is_some());
        assert!(parse_day("morgen früh").is_none());
        assert_eq!(highlight("Drucker <b> druckt", &["drucker".into()]), "<mark>Drucker</mark> &lt;b&gt; druckt");
        let long = format!("{} Toner leer {}", "a".repeat(100), "b".repeat(200));
        let e = excerpt(&long, &["toner".into()]).unwrap();
        assert!(e.starts_with("… ") && e.contains("<mark>Toner</mark>") && e.ends_with(" …"));
    }
}
