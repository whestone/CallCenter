//! Ticketlisten der Ansichten: Ad-hoc-Filter, Sortierung, Seiten, Massenbearbeitung.

use std::collections::HashMap;

use askama::Template;
use axum::{
    response::{Html, Response},
    routing::{get, post},
    Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::extract::{Form, Path, Query, State};
use super::*;
use crate::{
    auth::WebAgent,
    service::{self, TicketChanges},
};

const PER_PAGE: usize = 50;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/views", get(views_index))
        .route("/views/sidebar", get(views_sidebar))
        .route("/views/{id}", get(view_show))
        .route("/tickets/bulk", post(bulk_update))
}

pub struct ViewLink {
    pub id: i64,
    pub title: String,
    pub count: usize,
    pub active: bool,
}

/// Filter- und Sortierparameter einer Liste (alles Strings, damit kaputte
/// Werte einfach ignoriert werden statt einen Fehler auszulösen).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ListParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    // Rückmeldung der Massenbearbeitung
    #[serde(skip_serializing)]
    pub bulk_ok: Option<usize>,
    #[serde(skip_serializing)]
    pub bulk_err: Option<usize>,
}

impl ListParams {
    fn get(v: &Option<String>) -> Option<&str> {
        v.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    fn has_filters(&self) -> bool {
        [&self.q, &self.status, &self.priority, &self.assignee, &self.group, &self.channel]
            .iter()
            .any(|v| Self::get(v).is_some())
    }

    /// Query-String ohne leere Werte, optional mit geänderten Feldern.
    fn query(&self, f: impl FnOnce(&mut ListParams)) -> String {
        let mut p = self.clone();
        f(&mut p);
        for v in [&mut p.q, &mut p.status, &mut p.priority, &mut p.assignee, &mut p.group, &mut p.channel, &mut p.sort, &mut p.dir, &mut p.page] {
            if v.as_deref().is_some_and(|s| s.trim().is_empty()) {
                *v = None;
            }
        }
        serde_urlencoded::to_string(&p).unwrap_or_default()
    }

    fn matches(&self, t: &Ticket, me: &User) -> bool {
        if let Some(s) = Self::get(&self.status) {
            let ok = match s {
                "unsolved" => matches!(t.status.as_str(), "new" | "open" | "pending" | "hold"),
                s => t.status == s,
            };
            if !ok {
                return false;
            }
        }
        if let Some(p) = Self::get(&self.priority) {
            let ok = match p {
                "none" => t.priority.is_none(),
                p => t.priority.as_deref() == Some(p),
            };
            if !ok {
                return false;
            }
        }
        if let Some(a) = Self::get(&self.assignee) {
            let ok = match a {
                "me" => t.assignee_id == Some(me.id),
                "none" => t.assignee_id.is_none(),
                id => id.parse().ok() == t.assignee_id,
            };
            if !ok {
                return false;
            }
        }
        if let Some(g) = Self::get(&self.group) {
            let ok = match g {
                "none" => t.group_id.is_none(),
                id => id.parse().ok() == t.group_id,
            };
            if !ok {
                return false;
            }
        }
        if let Some(c) = Self::get(&self.channel) {
            if t.channel != c {
                return false;
            }
        }
        if let Some(q) = Self::get(&self.q) {
            let q = q.trim_start_matches('#').to_lowercase();
            if !(t.subject.to_lowercase().contains(&q) || t.id.to_string() == q) {
                return false;
            }
        }
        true
    }
}

struct Header {
    label: &'static str,
    url: String,
    arrow: &'static str,
}

struct PageLink {
    label: String,
    url: String,
    current: bool,
}

#[derive(Template)]
#[template(path = "views.html")]
struct ViewsPage {
    nav: Nav,
    views: Vec<ViewLink>,
    current: i64,
    current_title: String,
    rows: Vec<TicketRow>,
    total: usize,
    total_unfiltered: usize,
    filtered: bool,
    q: String,
    status_opts: Vec<Opt>,
    priority_opts: Vec<Opt>,
    assignee_opts: Vec<Opt>,
    group_opts: Vec<Opt>,
    channel_opts: Vec<Opt>,
    sort: String,
    dir: String,
    headers: Vec<Header>,
    pages: Vec<PageLink>,
    return_to: String,
    notice: String,
    bulk_status_opts: Vec<Opt>,
    bulk_priority_opts: Vec<Opt>,
    bulk_assignee_opts: Vec<Opt>,
    bulk_group_opts: Vec<Opt>,
    bulk_macro_opts: Vec<Opt>,
}

#[derive(Template)]
#[template(path = "views_sidebar.html")]
struct ViewsSidebar {
    views: Vec<ViewLink>,
    current: i64,
}

async fn active_views(db: &PgPool) -> AppResult<Vec<View>> {
    Ok(sqlx::query_as("SELECT * FROM views WHERE active ORDER BY position, id").fetch_all(db).await?)
}

async fn view_links(db: &PgPool, me: &User, current: i64) -> AppResult<(Vec<View>, Vec<Ticket>, Vec<ViewLink>)> {
    let views = active_views(db).await?;
    let tickets = service::candidate_tickets(db).await?;
    let now = Utc::now();
    let links = views
        .iter()
        .map(|v| ViewLink {
            id: v.id,
            title: v.title.clone(),
            count: service::count_view(v, &tickets, me, now),
            active: v.id == current,
        })
        .collect();
    Ok((views, tickets, links))
}

async fn views_index(State(st): State<AppState>, WebAgent(_): WebAgent) -> WebResult<Response> {
    let views = active_views(&st.db).await?;
    match views.first() {
        Some(v) => Ok(see_other(&format!("/views/{}", v.id))),
        None => Ok(see_other("/search")),
    }
}

/// Sortierschlüssel; `None` sortiert immer ans Ende.
fn sort_tickets(tickets: &mut [Ticket], sort: &str, desc: bool, policies: &HashMap<i64, SlaPolicy>, now: DateTime<Utc>) {
    let sla_due = |t: &Ticket| {
        t.sla_policy_id
            .and_then(|id| policies.get(&id))
            .and_then(|p| sla::next_target(p, t, now))
            .map(|s| s.due_at)
    };
    let prio = |t: &Ticket| t.priority.as_deref().and_then(priority_rank);
    tickets.sort_by(|a, b| {
        use std::cmp::Ordering;
        let ord = match sort {
            "id" => a.id.cmp(&b.id),
            "subject" => a.subject.to_lowercase().cmp(&b.subject.to_lowercase()),
            "status" => status_rank(&a.status).cmp(&status_rank(&b.status)),
            "priority" => match (prio(a), prio(b)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => return Ordering::Less,
                (None, Some(_)) => return Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
            "created" => a.created_at.cmp(&b.created_at),
            "sla" => match (sla_due(a), sla_due(b)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => return Ordering::Less,
                (None, Some(_)) => return Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
            _ => a.updated_at.cmp(&b.updated_at),
        };
        if desc { ord.reverse() } else { ord }
    });
}

async fn view_show(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path(id): Path<i64>,
    Query(p): Query<ListParams>,
) -> WebResult<Html<String>> {
    let db = &st.db;
    let now = Utc::now();
    let (views, tickets, links) = view_links(db, &me, id).await?;
    let view = views.iter().find(|v| v.id == id).ok_or(AppError::NotFound)?;
    let in_view = service::filter_view(view, &tickets, &me, now);
    let total_unfiltered = in_view.len();
    let mut matching: Vec<Ticket> = in_view.into_iter().filter(|t| p.matches(t, &me)).collect();

    // Ohne explizite Sortierung gilt die Sortierung der Ansicht.
    let sort = ListParams::get(&p.sort).unwrap_or("").to_string();
    let dir = if ListParams::get(&p.dir) == Some("asc") { "asc" } else { "desc" }.to_string();
    if !sort.is_empty() {
        let policies: HashMap<i64, SlaPolicy> = sla::load_policies_with_schedule(db).await?.into_iter().map(|p| (p.id, p)).collect();
        sort_tickets(&mut matching, &sort, dir == "desc", &policies, now);
    }

    let total = matching.len();
    let page_count = total.div_ceil(PER_PAGE).max(1);
    let page_no = ListParams::get(&p.page).and_then(|s| s.parse::<usize>().ok()).unwrap_or(1).clamp(1, page_count);
    let page_items: Vec<Ticket> = matching.into_iter().skip((page_no - 1) * PER_PAGE).take(PER_PAGE).collect();

    let base = format!("/views/{id}");
    let headers = [
        ("id", "Ticket"),
        ("priority", "Priorität"),
        ("", "Bearbeiter"),
        ("sla", "SLA"),
        ("updated", "Aktualisiert"),
    ]
    .into_iter()
    .map(|(key, label)| {
        if key.is_empty() {
            return Header { label, url: String::new(), arrow: "" };
        }
        let active = sort == key;
        // Erster Klick: sinnvolle Richtung (Priorität/Aktualisiert absteigend, sonst aufsteigend).
        let next_dir = match (active, dir.as_str()) {
            (true, "desc") => "asc",
            (true, _) => "desc",
            (false, _) if matches!(key, "priority" | "updated") => "desc",
            _ => "asc",
        };
        Header {
            label,
            url: format!("{base}?{}", p.query(|q| {
                q.sort = Some(key.into());
                q.dir = Some(next_dir.into());
                q.page = None;
            })),
            arrow: match (active, dir.as_str()) {
                (true, "asc") => " ▲",
                (true, _) => " ▼",
                _ => "",
            },
        }
    })
    .collect();

    let pages = if page_count > 1 {
        (1..=page_count)
            .map(|n| PageLink {
                label: n.to_string(),
                url: format!("{base}?{}", p.query(|q| q.page = Some(n.to_string()))),
                current: n == page_no,
            })
            .collect()
    } else {
        Vec::new()
    };

    let agents: Vec<User> =
        sqlx::query_as("SELECT * FROM users WHERE role IN ('agent', 'admin') AND active ORDER BY name")
            .fetch_all(db)
            .await?;
    let groups: Vec<Group> = sqlx::query_as("SELECT * FROM agent_groups ORDER BY name").fetch_all(db).await?;
    let macros: Vec<Macro> = sqlx::query_as(
        "SELECT * FROM macros WHERE active AND (owner_id IS NULL OR owner_id = $1) ORDER BY owner_id IS NOT NULL, position, id",
    )
    .bind(me.id)
    .fetch_all(db)
        .await?;

    let sel = |v: &Option<String>| ListParams::get(v).map(str::to_string);
    let with_special = |special: &[(&str, &str)], rest: Vec<(String, String)>, selected: Option<String>, blank: &str| {
        let mut out = vec![Opt { value: String::new(), label: blank.into(), selected: selected.is_none() }];
        out.extend(
            special
                .iter()
                .map(|(v, l)| ((*v).to_string(), (*l).to_string()))
                .chain(rest)
                .map(|(value, label)| Opt { selected: selected.as_deref() == Some(value.as_str()), value, label }),
        );
        out
    };
    let agent_list: Vec<(String, String)> = agents.iter().map(|a| (a.id.to_string(), a.name.clone())).collect();
    let group_list: Vec<(String, String)> = groups.iter().map(|g| (g.id.to_string(), g.name.clone())).collect();
    let enum_list = |values: &[&str], label: fn(&str) -> &'static str| -> Vec<(String, String)> {
        values.iter().map(|v| ((*v).to_string(), label(v).to_string())).collect()
    };

    let notice = match (p.bulk_ok, p.bulk_err) {
        (Some(ok), Some(err)) if err > 0 => format!("{ok} Tickets aktualisiert, {err} konnten nicht geändert werden (z. B. geschlossen)."),
        (Some(ok), _) => format!("{ok} Tickets aktualisiert."),
        _ => String::new(),
    };

    page(ViewsPage {
        nav: Nav::new(&me, "views"),
        views: links,
        current: id,
        current_title: view.title.clone(),
        rows: ticket_rows(db, &page_items).await?,
        total,
        total_unfiltered,
        filtered: p.has_filters(),
        q: ListParams::get(&p.q).unwrap_or("").to_string(),
        status_opts: with_special(&[("unsolved", "Ungelöst")], enum_list(&STATUSES, status_label), sel(&p.status), "Status: alle"),
        priority_opts: with_special(&[("none", "ohne Priorität")], enum_list(&PRIORITIES, priority_label), sel(&p.priority), "Priorität: alle"),
        assignee_opts: with_special(&[("me", "Mir zugewiesen"), ("none", "Nicht zugewiesen")], agent_list.clone(), sel(&p.assignee), "Bearbeiter: alle"),
        group_opts: with_special(&[("none", "ohne Gruppe")], group_list.clone(), sel(&p.group), "Gruppe: alle"),
        channel_opts: with_special(&[], enum_list(&CHANNELS, channel_label), sel(&p.channel), "Kanal: alle"),
        sort,
        dir,
        headers,
        pages,
        return_to: format!("{base}?{}", p.query(|_| {})),
        notice,
        bulk_status_opts: with_special(&[], enum_list(&["open", "pending", "hold", "solved"], status_label), None, "Status …"),
        bulk_priority_opts: with_special(&[], enum_list(&PRIORITIES, priority_label), None, "Priorität …"),
        bulk_assignee_opts: with_special(&[("me", "Mir zuweisen"), ("none", "Zuweisung entfernen")], agent_list, None, "Bearbeiter …"),
        bulk_group_opts: with_special(&[], group_list, None, "Gruppe …"),
        bulk_macro_opts: with_special(&[], macros.iter().map(|m| (m.id.to_string(), m.title.clone())).collect(), None, "Makro …"),
    })
}

#[derive(Deserialize)]
struct SidebarQuery {
    current: Option<i64>,
}

async fn views_sidebar(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Query(q): Query<SidebarQuery>,
) -> WebResult<Html<String>> {
    let current = q.current.unwrap_or(0);
    let (_, _, links) = view_links(&st.db, &me, current).await?;
    page(ViewsSidebar { views: links, current })
}

/// Nur lokale Pfade als Rücksprungziel zulassen (kein Open Redirect).
fn safe_return(path: Option<&str>) -> String {
    match path {
        Some(p) if p.starts_with('/') && !p.starts_with("//") && !p.contains('\\') => p.to_string(),
        _ => "/views".into(),
    }
}

/// Massenbearbeitung: dieselben Änderungen (und/oder ein Makro) auf alle markierten Tickets.
async fn bulk_update(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Form(pairs): Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    let value = |key: &str| {
        pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.trim()).filter(|v| !v.is_empty())
    };
    let ids: Vec<i64> = pairs.iter().filter(|(k, _)| k == "ticket_id").filter_map(|(_, v)| v.parse().ok()).collect();
    let return_to = safe_return(value("return_to"));
    if ids.is_empty() {
        return Ok(see_other(&return_to));
    }
    let macro_id = value("macro_id").and_then(|v| v.parse::<i64>().ok());
    let changes = TicketChanges {
        status: value("status").map(Into::into),
        priority: value("priority").map(|p| Some(p.to_string())),
        assignee_id: value("assignee_id").map(|a| match a {
            "me" => Some(me.id),
            "none" => None,
            id => id.parse().ok(),
        }),
        group_id: value("group_id").map(|g| g.parse().ok()),
        ..Default::default()
    };

    let (mut ok, mut err) = (0usize, 0usize);
    for id in ids {
        match service::update_ticket(&st.db, &me, id, changes.clone(), macro_id, "bulk").await {
            Ok(_) => ok += 1,
            Err(e) => {
                tracing::info!(ticket = id, "Massenbearbeitung übersprungen: {}", e.public_message());
                err += 1;
            }
        }
    }
    let sep = if return_to.contains('?') { '&' } else { '?' };
    Ok(see_other(&format!("{return_to}{sep}bulk_ok={ok}&bulk_err={err}")))
}
