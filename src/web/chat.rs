//! Live-Chat: öffentliche Besucher-Schnittstelle (Widget/iframe) und Chat-Übersicht für Agenten.

use askama::Template;
use axum::{
    http::HeaderMap,
    response::{Html, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{Local, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use super::extract::{Path, Query, State};
use super::*;
use crate::{
    auth::WebAgent,
    chat::{self, ChatSession},
    error::api_extract,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/chat/embed", get(embed))
        .route("/chat/demo", get(demo))
        .route("/chat/api/status", get(api_status))
        .route("/chat/api/start", post(api_start))
        .route("/chat/api/poll", get(api_poll))
        .route("/chat/api/send", post(api_send))
        .route("/chat/api/typing", post(api_typing))
        .route("/chat/api/end", post(api_end))
        .route("/chats", get(chats_index))
        .route("/chats/badge", get(chats_badge))
        .route("/chats/{id}/accept", post(accept))
        .route("/chats/{id}/end", post(end_by_agent))
}

// ---------------------------------------------------------------------------
// Besucher (ohne Anmeldung, Token im Header X-Chat-Token)
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "chat_embed.html")]
struct EmbedPage;

async fn embed() -> WebResult<Html<String>> {
    page(EmbedPage)
}

#[derive(Template)]
#[template(path = "chat_demo.html")]
struct DemoPage;

async fn demo() -> WebResult<Html<String>> {
    page(DemoPage)
}

async fn session(db: &PgPool, headers: &HeaderMap) -> AppResult<ChatSession> {
    let token = headers.get("x-chat-token").and_then(|v| v.to_str().ok()).ok_or(AppError::NotFound)?;
    chat::by_token(db, token).await
}

async fn api_status(State(st): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "online": chat::agents_online(&st.db).await? })))
}

#[derive(Deserialize)]
struct StartBody {
    #[serde(default)]
    name: String,
    email: String,
    message: String,
    #[serde(default)]
    page_url: String,
}

async fn api_start(State(st): State<AppState>, api_extract::Json(b): api_extract::Json<StartBody>) -> AppResult<Json<Value>> {
    let started = chat::start(&st.db, &b.name, &b.email, &b.message, &b.page_url).await?;
    Ok(Json(json!({ "token": started.token, "status": started.session.status })))
}

#[derive(Deserialize)]
struct PollQuery {
    after: Option<i64>,
}

async fn api_poll(
    State(st): State<AppState>,
    headers: HeaderMap,
    api_extract::Query(q): api_extract::Query<PollQuery>,
) -> AppResult<Json<Value>> {
    let s = session(&st.db, &headers).await?;
    let now = Utc::now();
    let agent: Option<String> = match s.agent_id {
        Some(id) => sqlx::query_scalar("SELECT name FROM users WHERE id = $1").bind(id).fetch_optional(&st.db).await?,
        None => None,
    };
    let messages: Vec<Value> = chat::messages(&st.db, s.ticket_id, q.after.unwrap_or(0))
        .await?
        .into_iter()
        .map(|m| {
            json!({
                "id": m.id,
                "author": m.author,
                "from_agent": m.from_agent,
                "html": crate::markdown::render(&m.body),
                "time": m.created_at.with_timezone(&Local).format("%H:%M").to_string(),
            })
        })
        .collect();
    Ok(Json(json!({
        "status": s.status,
        "agent": agent,
        "agent_typing": chat::typing(s.agent_typing_at, now),
        "messages": messages,
    })))
}

#[derive(Deserialize)]
struct SendBody {
    body: String,
}

async fn api_send(
    State(st): State<AppState>,
    headers: HeaderMap,
    api_extract::Json(b): api_extract::Json<SendBody>,
) -> AppResult<Json<Value>> {
    let s = session(&st.db, &headers).await?;
    chat::send(&st.db, &s, &b.body).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn api_typing(State(st): State<AppState>, headers: HeaderMap) -> AppResult<Json<Value>> {
    let s = session(&st.db, &headers).await?;
    chat::visitor_typing(&st.db, &s).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn api_end(State(st): State<AppState>, headers: HeaderMap) -> AppResult<Json<Value>> {
    let s = session(&st.db, &headers).await?;
    let visitor: User = sqlx::query_as("SELECT * FROM users WHERE id = $1").bind(s.visitor_id).fetch_one(&st.db).await?;
    chat::end(&st.db, &s, &visitor).await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Agenten
// ---------------------------------------------------------------------------

struct ChatRow {
    id: i64,
    ticket_id: i64,
    subject: String,
    visitor: String,
    email: String,
    status: String,
    status_label: &'static str,
    agent: String,
    mine: bool,
    since: String,
    last: String,
    last_from_visitor: bool,
    present: bool,
    page_url: String,
}

#[derive(Template)]
#[template(path = "chats.html")]
struct ChatsPage {
    nav: Nav,
    rows: Vec<ChatRow>,
    snippet: String,
    demo_url: String,
    online: bool,
}

async fn chats_index(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Html<String>> {
    let sessions = chat::open_sessions(&st.db).await?;
    let now = Utc::now();
    let mut rows = Vec::new();
    for s in sessions {
        let (subject, visitor, email): (String, String, Option<String>) = sqlx::query_as(
            "SELECT t.subject, u.name, u.email FROM tickets t JOIN users u ON u.id = $2 WHERE t.id = $1",
        )
        .bind(s.ticket_id)
        .bind(s.visitor_id)
        .fetch_one(&st.db)
        .await?;
        let last: Option<(String, String)> = sqlx::query_as(
            "SELECT c.body, u.role FROM ticket_comments c JOIN users u ON u.id = c.author_id
             WHERE c.ticket_id = $1 AND c.public ORDER BY c.id DESC LIMIT 1",
        )
        .bind(s.ticket_id)
        .fetch_optional(&st.db)
        .await?;
        let agent = match s.agent_id {
            Some(id) => sqlx::query_scalar("SELECT name FROM users WHERE id = $1").bind(id).fetch_optional(&st.db).await?.unwrap_or_default(),
            None => String::new(),
        };
        let (last_text, last_role) = last.unwrap_or_default();
        rows.push(ChatRow {
            id: s.id,
            ticket_id: s.ticket_id,
            subject,
            visitor,
            email: email.unwrap_or_default(),
            status_label: chat::status_label(&s.status),
            agent,
            mine: s.agent_id == Some(me.id),
            since: fmt_rel(s.created_at, now),
            last: last_text.chars().take(120).collect(),
            last_from_visitor: last_role == "end-user",
            present: chat::visitor_present(&s, now),
            page_url: s.page_url.clone(),
            status: s.status,
        });
    }
    let base = crate::email::public_url();
    page(ChatsPage {
        nav: Nav::new(&me, "chats"),
        rows,
        snippet: format!(r#"<script src="{base}/static/chat-widget.js" async></script>"#),
        demo_url: format!("{base}/chat/demo"),
        online: me.availability == "online",
    })
}

async fn chats_badge(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Html<String>> {
    let n = chat::attention_count(&st.db, &me).await?;
    Ok(Html(if n > 0 { format!(r#"<span class="rail-badge">{n}</span>"#) } else { String::new() }))
}

async fn accept(State(st): State<AppState>, WebAgent(me): WebAgent, Path(id): Path<i64>) -> WebResult<Response> {
    let s = chat::accept(&st.db, id, &me).await?;
    Ok(see_other(&format!("/tickets/{}", s.ticket_id)))
}

#[derive(Deserialize)]
struct BackQuery {
    back: Option<String>,
}

async fn end_by_agent(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path(id): Path<i64>,
    Query(q): Query<BackQuery>,
) -> WebResult<Response> {
    let s = chat::get(&st.db, id).await?;
    chat::end(&st.db, &s, &me).await?;
    Ok(see_other(&match q.back.as_deref() {
        Some("ticket") => format!("/tickets/{}", s.ticket_id),
        _ => "/chats".into(),
    }))
}
