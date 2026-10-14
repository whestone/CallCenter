//! Benachrichtigungen (Glocke) und Erwähnungs-Vorschläge für das Antwortfeld.

use askama::Template;
use axum::{
    response::{Html, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::extract::{Path, Query, State};
use super::*;
use crate::{auth::WebAgent, notify};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/notifications", get(index))
        .route("/notifications/badge", get(badge))
        .route("/notifications/read-all", post(read_all))
        .route("/notifications/{id}", get(open))
        .route("/agents/mention", get(mention_lookup))
}

#[derive(Template)]
#[template(path = "notifications_badge.html")]
struct Badge {
    count: i64,
}

async fn badge(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Html<String>> {
    page(Badge { count: notify::unread_count(&st.db, me.id).await? })
}

struct Row {
    id: i64,
    kind: &'static str,
    kind_key: String,
    body: String,
    unread: bool,
    created: String,
}

#[derive(Template)]
#[template(path = "notifications.html")]
struct IndexPage {
    nav: Nav,
    rows: Vec<Row>,
    unread: usize,
}

async fn index(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Html<String>> {
    let now = chrono::Utc::now();
    let rows: Vec<Row> = notify::list(&st.db, me.id, 100)
        .await?
        .into_iter()
        .map(|n| Row {
            id: n.id,
            kind: notify::kind_label(&n.kind),
            kind_key: n.kind.clone(),
            body: n.body,
            unread: n.read_at.is_none(),
            created: fmt_rel(n.created_at, now),
        })
        .collect();
    page(IndexPage { nav: Nav::new(&me, "notifications"), unread: rows.iter().filter(|r| r.unread).count(), rows })
}

async fn open(State(st): State<AppState>, WebAgent(me): WebAgent, Path(id): Path<i64>) -> WebResult<Response> {
    Ok(match notify::mark_read(&st.db, me.id, id).await? {
        Some(ticket) => see_other(&format!("/tickets/{ticket}")),
        None => see_other("/notifications"),
    })
}

async fn read_all(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Response> {
    notify::mark_all_read(&st.db, me.id).await?;
    Ok(see_other("/notifications"))
}

#[derive(Deserialize)]
struct MentionQuery {
    q: Option<String>,
}

/// Vorschläge für „@Name“ im Antwortfeld.
async fn mention_lookup(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Query(q): Query<MentionQuery>,
) -> WebResult<Json<Value>> {
    let term = q.q.unwrap_or_default().trim().to_lowercase();
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM users WHERE role IN ('agent', 'admin') AND active AND id <> $1 AND email <> $2
           AND lower(name) LIKE '%' || $3 || '%' ORDER BY lower(name) LIKE $3 || '%' DESC, name LIMIT 8",
    )
    .bind(me.id)
    .bind(crate::automations::SYSTEM_EMAIL)
    .bind(term.replace(['%', '_'], ""))
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(names)))
}
