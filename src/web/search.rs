//! Suche: Ergebnisseite (Tickets mit Textausschnitt, Kunden, Firmen, Wissensartikel)
//! und Live-Vorschläge für das Suchfeld in der Kopfzeile.

use askama::Template;
use axum::{
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Deserialize;

use super::extract::{Query, State};
use super::*;
use crate::{auth::WebAgent, search, service};

pub fn router() -> Router<AppState> {
    Router::new().route("/search", get(results)).route("/search/quick", get(quick))
}

#[derive(Deserialize)]
struct SearchParams {
    q: Option<String>,
    sort: Option<String>,
}

/// `#123` oder `123` → direkt zum Ticket, falls es existiert.
async fn direct_ticket(db: &PgPool, q: &str) -> AppResult<Option<i64>> {
    let Ok(id) = q.trim().trim_start_matches('#').parse::<i64>() else { return Ok(None) };
    Ok(sqlx::query_scalar("SELECT id FROM tickets WHERE id = $1").bind(id).fetch_optional(db).await?)
}

pub struct Hit {
    pub row: TicketRow,
    pub snippet: String,
}

pub struct PersonHit {
    pub id: i64,
    pub name: String,
    pub contact: String,
    pub org: String,
}

pub struct OrgHit {
    pub id: i64,
    pub name: String,
    pub domains: String,
}

pub struct ArticleHit {
    pub id: i64,
    pub title: String,
    pub draft: bool,
}

async fn people(db: &PgPool, q: &str, limit: i64) -> AppResult<Vec<PersonHit>> {
    let users = service::search_users(db, q, limit, true).await?;
    let orgs: HashMap<i64, String> = sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM organizations")
        .fetch_all(db)
        .await?
        .into_iter()
        .collect();
    Ok(users
        .into_iter()
        .map(|u| PersonHit {
            id: u.id,
            contact: [u.email.clone(), u.phone.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · "),
            org: u.organization_id.and_then(|id| orgs.get(&id).cloned()).unwrap_or_default(),
            name: u.name,
        })
        .collect())
}

async fn organizations(db: &PgPool, q: &str, limit: i64) -> AppResult<Vec<OrgHit>> {
    let p = format!("%{}%", q.trim().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let rows: Vec<(i64, String, Vec<String>)> = sqlx::query_as(
        "SELECT id, name, domain_names FROM organizations
         WHERE name ILIKE $1 OR array_to_string(domain_names, ' ') ILIKE $1 ORDER BY name LIMIT $2",
    )
    .bind(p)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|(id, name, d)| OrgHit { id, name, domains: d.join(", ") }).collect())
}

async fn articles(db: &PgPool, q: &str, limit: i64) -> AppResult<Vec<ArticleHit>> {
    // Ohne verwertbare Suchwörter liefert die KB-Suche die neuesten Artikel – hier unerwünscht.
    if !q.split(|c: char| !c.is_alphanumeric()).any(|w| w.chars().count() >= 3) {
        return Ok(Vec::new());
    }
    Ok(crate::kb::search(db, q, false, false, limit)
        .await?
        .into_iter()
        .map(|a| ArticleHit { id: a.id, draft: a.status != "published", title: a.title })
        .collect())
}

/// Reine Filteranfragen (nur `schlüssel:wert`) suchen keine Personen/Firmen/Artikel.
fn free_text(q: &str) -> String {
    search::tokenize(q)
        .into_iter()
        .filter(|t| t.key.is_empty() && !t.negate)
        .map(|t| t.value)
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Template)]
#[template(path = "search.html")]
struct ResultsPage {
    nav: Nav,
    q: String,
    sort_opts: Vec<Opt>,
    hits: Vec<Hit>,
    more: bool,
    people: Vec<PersonHit>,
    orgs: Vec<OrgHit>,
    articles: Vec<ArticleHit>,
    problems: Vec<String>,
    examples: Vec<(&'static str, &'static str)>,
}

const EXAMPLES: [(&str, &str); 8] = [
    ("status:unsolved assignee:me", "Meine offenen Tickets"),
    ("assignee:none status:unsolved", "Nicht zugewiesen"),
    ("created:heute", "Heute erstellt"),
    ("priority:urgent status:unsolved", "Dringend & offen"),
    ("channel:phone created>7d", "Anrufe der letzten 7 Tage"),
    ("status:pending updated<3d", "Wartend, seit 3 Tagen unverändert"),
    ("\"kein internet\" -status:closed", "Phrase, ohne geschlossene"),
    ("tag:verpasster_anruf status:unsolved", "Offene Rückrufe"),
];

async fn results(State(st): State<AppState>, WebAgent(me): WebAgent, Query(p): Query<SearchParams>) -> WebResult<Response> {
    let db = &st.db;
    let q = p.q.unwrap_or_default();
    if let Some(id) = direct_ticket(db, &q).await? {
        return Ok(see_other(&format!("/tickets/{id}")));
    }
    let sort = p.sort.filter(|s| search::SORTS.iter().any(|(k, _)| k == s)).unwrap_or_else(|| "relevance".into());
    let tokens = search::tokenize(&q);
    let (mut tickets, people, orgs, articles) = if q.trim().is_empty() {
        (Vec::new(), Vec::new(), Vec::new(), Vec::new())
    } else {
        let text = free_text(&q);
        let others = !text.is_empty() && tokens.iter().all(|t| t.key.is_empty() && !t.negate);
        (
            search::tickets(db, &q, &me, &sort).await?,
            if others { people(db, &text, 20).await? } else { Vec::new() },
            if others { organizations(db, &text, 10).await? } else { Vec::new() },
            if others { articles(db, &text, 5).await? } else { Vec::new() },
        )
    };
    let more = tickets.len() > search::LIMIT;
    tickets.truncate(search::LIMIT);
    let terms = search::text_terms(&tokens);
    let mut snippets = search::snippets(db, &tickets, &terms).await?;
    let hits = ticket_rows(db, &tickets)
        .await?
        .into_iter()
        .map(|mut row| {
            let snippet = snippets.remove(&row.id).unwrap_or_default();
            row.subject_html = search::highlight(&row.subject, &terms);
            Hit { row, snippet }
        })
        .collect();
    Ok(page(ResultsPage {
        nav: Nav::new(&me, "search"),
        sort_opts: search::SORTS
            .iter()
            .map(|(k, l)| Opt { value: k.to_string(), label: l.to_string(), selected: *k == sort })
            .collect(),
        problems: search::problems(&tokens),
        q,
        hits,
        more,
        people,
        orgs,
        articles,
        examples: EXAMPLES.to_vec(),
    })?
    .into_response())
}

// ---------------------------------------------------------------------------
// Live-Vorschläge (Dropdown unter dem Suchfeld)
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "search_quick.html")]
struct QuickFragment {
    q: String,
    q_url: String,
    tickets: Vec<(i64, String, String, &'static str)>,
    people: Vec<PersonHit>,
    orgs: Vec<OrgHit>,
    articles: Vec<ArticleHit>,
}

async fn quick(State(st): State<AppState>, WebAgent(me): WebAgent, Query(p): Query<SearchParams>) -> WebResult<Response> {
    let db = &st.db;
    let q = p.q.unwrap_or_default().trim().to_string();
    if q.chars().count() < 2 {
        return Ok(Html(String::new()).into_response());
    }
    let terms = search::text_terms(&search::tokenize(&q));
    let mut tickets = search::tickets(db, &q, &me, "relevance").await?;
    tickets.truncate(6);
    let text = free_text(&q);
    let filters_only = text.is_empty() || q.contains(':');
    Ok(page(QuickFragment {
        q_url: serde_urlencoded::to_string([("q", q.as_str())]).unwrap_or_default(),
        tickets: tickets
            .iter()
            .map(|t| (t.id, search::highlight(&t.subject, &terms), t.status.clone(), status_label(&t.status)))
            .collect(),
        people: if filters_only { Vec::new() } else { people(db, &text, 4).await? },
        orgs: if filters_only { Vec::new() } else { organizations(db, &text, 3).await? },
        articles: if filters_only { Vec::new() } else { articles(db, &text, 3).await? },
        q,
    })?
    .into_response())
}
