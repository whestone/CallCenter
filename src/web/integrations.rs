//! Admin: Webhooks verwalten, testen und Zustellungen einsehen.

use askama::Template;
use axum::{
    response::{Html, Response},
    routing::{get, post},
    Router,
};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::extract::{Form, Path, State};
use super::*;
use crate::{auth::WebAdmin, webhooks};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/webhooks", get(index).post(create))
        .route("/admin/webhooks/{id}/ping", post(ping))
        .route("/admin/webhooks/{id}/toggle", post(toggle))
        .route("/admin/webhooks/{id}/delete", post(delete))
}

struct HookRow {
    id: i64,
    name: String,
    url: String,
    secret: String,
    events: String,
    active: bool,
}

struct DeliveryRow {
    hook: String,
    event: String,
    status: String,
    status_label: &'static str,
    detail: String,
    created: String,
}

struct EventOpt {
    key: &'static str,
    label: &'static str,
}

#[derive(Template)]
#[template(path = "admin/webhooks.html")]
struct WebhooksPage {
    nav: Nav,
    tab: &'static str,
    hooks: Vec<HookRow>,
    deliveries: Vec<DeliveryRow>,
    events: Vec<EventOpt>,
}

async fn index(State(st): State<AppState>, WebAdmin(me): WebAdmin) -> WebResult<Html<String>> {
    let hooks: Vec<(i64, String, String, String, Vec<String>, bool)> =
        sqlx::query_as("SELECT id, name, url, secret, events, active FROM webhooks ORDER BY id").fetch_all(&st.db).await?;
    #[allow(clippy::type_complexity)]
    let deliveries: Vec<(String, String, String, Option<i32>, Option<String>, i32, DateTime<Utc>)> = sqlx::query_as(
        "SELECT w.name, d.event, d.status, d.response_code, d.error, d.attempts, d.created_at
         FROM webhook_deliveries d JOIN webhooks w ON w.id = d.webhook_id ORDER BY d.id DESC LIMIT 30",
    )
    .fetch_all(&st.db)
    .await?;
    let label = |e: &str| webhooks::EVENTS.iter().find(|(k, _)| *k == e).map(|(_, l)| *l).unwrap_or("Test (ping)");
    let now = Utc::now();
    page(WebhooksPage {
        nav: Nav::new(&me, "admin"),
        tab: "webhooks",
        hooks: hooks
            .into_iter()
            .map(|(id, name, url, secret, events, active)| HookRow {
                id,
                name,
                url,
                secret,
                events: events.iter().map(|e| label(e)).collect::<Vec<_>>().join(", "),
                active,
            })
            .collect(),
        deliveries: deliveries
            .into_iter()
            .map(|(hook, event, status, code, error, attempts, created)| DeliveryRow {
                hook,
                event: label(&event).to_string(),
                status_label: match status.as_str() {
                    "sent" => "zugestellt",
                    "pending" => "wird wiederholt",
                    _ => "fehlgeschlagen",
                },
                detail: match (code, error) {
                    (_, Some(e)) => format!("{e} · Versuch {attempts}"),
                    (Some(c), None) => format!("HTTP {c}"),
                    _ => String::new(),
                },
                status,
                created: fmt_rel(created, now),
            })
            .collect(),
        events: webhooks::EVENTS.iter().map(|(key, label)| EventOpt { key, label }).collect(),
    })
}

async fn create(State(st): State<AppState>, WebAdmin(_): WebAdmin, Form(pairs): Form<Vec<(String, String)>>) -> WebResult<Response> {
    let value = |k: &str| pairs.iter().find(|(key, _)| key == k).map(|(_, v)| v.trim().to_string()).unwrap_or_default();
    let (name, url) = (value("name"), value("url"));
    if name.is_empty() {
        return Err(AppError::invalid("Bitte einen Namen angeben").into());
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) || url.contains(char::is_whitespace) {
        return Err(AppError::invalid("Die URL muss mit http:// oder https:// beginnen").into());
    }
    let events: Vec<String> = pairs
        .iter()
        .filter(|(k, v)| k == "event" && webhooks::EVENTS.iter().any(|(e, _)| e == v))
        .map(|(_, v)| v.clone())
        .collect();
    if events.is_empty() {
        return Err(AppError::invalid("Bitte mindestens ein Ereignis auswählen").into());
    }
    let secret = format!("whsec_{}", Uuid::new_v4().simple());
    sqlx::query("INSERT INTO webhooks (name, url, secret, events) VALUES ($1, $2, $3, $4)")
        .bind(name)
        .bind(url)
        .bind(secret)
        .bind(events)
        .execute(&st.db)
        .await?;
    Ok(see_other("/admin/webhooks"))
}

async fn ping(State(st): State<AppState>, WebAdmin(_): WebAdmin, Path(id): Path<i64>) -> WebResult<Response> {
    webhooks::ping(&st.db, id).await?;
    Ok(see_other("/admin/webhooks"))
}

async fn toggle(State(st): State<AppState>, WebAdmin(_): WebAdmin, Path(id): Path<i64>) -> WebResult<Response> {
    sqlx::query("UPDATE webhooks SET active = NOT active WHERE id = $1").bind(id).execute(&st.db).await?;
    Ok(see_other("/admin/webhooks"))
}

async fn delete(State(st): State<AppState>, WebAdmin(_): WebAdmin, Path(id): Path<i64>) -> WebResult<Response> {
    sqlx::query("DELETE FROM webhooks WHERE id = $1").bind(id).execute(&st.db).await?;
    Ok(see_other("/admin/webhooks"))
}
