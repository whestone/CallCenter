//! Telefonie in der Oberfläche: Anruf-Banner (Screen-Pop), Click-to-Call,
//! Admin-Seite mit Simulator und die Webhooks der Anbieter (sipgate, Twilio, generisch).

use std::collections::HashMap;

use askama::Template;
use axum::{
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;

use super::extract::{Form, Path, Query, State};
use super::*;
use crate::{
    auth::{WebAdmin, WebAgent},
    telephony::{self, Call, DialResult},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/calls/banner", get(banner))
        .route("/calls/dial", post(dial))
        .route("/calls/{id}/sim-answer", post(sim_answer))
        .route("/calls/{id}/sim-hangup", post(sim_hangup))
        .route("/admin/telephony", get(admin_page))
        .route("/admin/telephony/simulate", post(simulate))
        .route("/admin/telephony/secret", post(rotate_secret))
        .route("/admin/telephony/secret/delete", post(clear_secret))
        .route("/admin/telephony/sipgate", post(save_sipgate))
        .route("/telephony/sipgate", post(sipgate_webhook))
        .route("/telephony/twilio", post(twilio_webhook))
        .route("/api/v2/telephony/events", post(generic_webhook))
}

// ---------------------------------------------------------------------------
// Anzeige
// ---------------------------------------------------------------------------

pub struct CallView {
    pub id: i64,
    pub number: String,
    pub name: String,
    pub customer_id: i64,
    pub org: String,
    pub open_tickets: i64,
    pub status: String,
    pub status_label: &'static str,
    pub direction_label: &'static str,
    pub agent: String,
    pub ticket_id: i64,
    pub started: String,
    pub duration: String,
    pub recording_url: String,
    pub simulated: bool,
}

pub async fn call_views(db: &PgPool, calls: &[Call]) -> AppResult<Vec<CallView>> {
    let ids: Vec<i64> = calls.iter().flat_map(|c| [c.customer_id, c.agent_id]).flatten().collect();
    let names = user_names(db, ids).await?;
    let customers: Vec<i64> = calls.iter().filter_map(|c| c.customer_id).collect();
    let orgs: HashMap<i64, String> = sqlx::query_as::<_, (i64, String)>(
        "SELECT u.id, o.name FROM users u JOIN organizations o ON o.id = u.organization_id WHERE u.id = ANY($1)",
    )
    .bind(&customers)
    .fetch_all(db)
    .await?
    .into_iter()
    .collect();
    let open: HashMap<i64, i64> = sqlx::query_as::<_, (i64, i64)>(
        "SELECT requester_id, count(*) FROM tickets WHERE requester_id = ANY($1)
           AND status IN ('new', 'open', 'pending', 'hold') GROUP BY requester_id",
    )
    .bind(&customers)
    .fetch_all(db)
    .await?
    .into_iter()
    .collect();
    let now = Utc::now();
    Ok(calls
        .iter()
        .map(|c| CallView {
            id: c.id,
            number: c.remote_number().to_string(),
            name: c.customer_id.and_then(|id| names.get(&id).cloned()).unwrap_or_else(|| "Unbekannter Anrufer".into()),
            customer_id: c.customer_id.unwrap_or(0),
            org: c.customer_id.and_then(|id| orgs.get(&id).cloned()).unwrap_or_default(),
            open_tickets: c.customer_id.and_then(|id| open.get(&id).copied()).unwrap_or(0),
            status_label: telephony::status_label(&c.status),
            status: c.status.clone(),
            direction_label: if c.direction == "inbound" { "eingehend" } else { "ausgehend" },
            agent: c.agent_id.and_then(|id| names.get(&id).cloned()).unwrap_or_default(),
            ticket_id: c.ticket_id.unwrap_or(0),
            started: fmt_rel(c.started_at, now),
            duration: telephony::fmt_duration(c.duration_seconds),
            recording_url: c.recording_url.clone().unwrap_or_default(),
            simulated: c.provider == "simulator",
        })
        .collect())
}

#[derive(Template)]
#[template(path = "calls_banner.html")]
struct Banner {
    calls: Vec<CallView>,
}

/// htmx-Fragment (alle 3 s): klingelnde Anrufe und eigene laufende Gespräche.
async fn banner(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Html<String>> {
    // Abwesende/offline Agenten bekommen keine klingelnden Anrufe angezeigt.
    let mut calls = telephony::active_for(&st.db, &me).await?;
    if me.availability != "online" {
        calls.retain(|c| c.status == "answered");
    }
    page(Banner { calls: call_views(&st.db, &calls).await? })
}

// ---------------------------------------------------------------------------
// Click-to-Call
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct DialForm {
    number: String,
    ticket_id: Option<String>,
}

fn back(headers: &HeaderMap) -> String {
    headers
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.split_once("://").and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_string())))
        .unwrap_or_else(|| "/views".into())
}

async fn dial(State(st): State<AppState>, WebAgent(me): WebAgent, headers: HeaderMap, Form(f): Form<DialForm>) -> WebResult<Response> {
    match telephony::dial(&st.db, &me, &f.number).await? {
        DialResult::Started(call) => {
            if let Some(ticket_id) = parse_id(&f.ticket_id) {
                telephony::link_ticket(&st.db, call.id, ticket_id, &me).await?;
            }
            Ok(see_other(&back(&headers)))
        }
        // Ohne Anbieter: Softphone/Telefon des Rechners über tel:-Link.
        DialResult::TelLink(link) => Ok(([("HX-Redirect", link.clone())], Redirect::to(&link)).into_response()),
    }
}

// ---------------------------------------------------------------------------
// Simulator (Testen ohne Anbieter)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SimForm {
    from: String,
}

async fn simulate(State(st): State<AppState>, WebAdmin(_): WebAdmin, Form(f): Form<SimForm>) -> WebResult<Response> {
    let id = format!("sim-{}", uuid::Uuid::new_v4().simple());
    telephony::call_started(&st.db, "simulator", &id, "inbound", &f.from, "+4930999000", None).await?;
    Ok(see_other("/admin/telephony"))
}

async fn sim_call(db: &PgPool, id: i64) -> AppResult<Call> {
    let call = telephony::get(db, id).await?;
    if call.provider != "simulator" {
        return Err(AppError::invalid("Nur simulierte Anrufe lassen sich hier steuern"));
    }
    Ok(call)
}

async fn sim_answer(State(st): State<AppState>, WebAgent(me): WebAgent, headers: HeaderMap, Path(id): Path<i64>) -> WebResult<Response> {
    let call = sim_call(&st.db, id).await?;
    telephony::call_answered(&st.db, "simulator", &call.provider_call_id, Some(&me)).await?;
    Ok(see_other(&back(&headers)))
}

async fn sim_hangup(State(st): State<AppState>, WebAgent(_): WebAgent, headers: HeaderMap, Path(id): Path<i64>) -> WebResult<Response> {
    let call = sim_call(&st.db, id).await?;
    let cause = if call.status == "ringing" { "noAnswer" } else { "normalClearing" };
    telephony::call_ended(&st.db, "simulator", &call.provider_call_id, Some(cause), None, None).await?;
    Ok(see_other(&back(&headers)))
}

// ---------------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------------

struct ExtensionRow {
    name: String,
    extension: String,
    id: i64,
}

#[derive(Template)]
#[template(path = "admin/telephony.html")]
struct AdminPage {
    nav: Nav,
    tab: &'static str,
    secret_ok: bool,
    secret_in_admin: bool,
    secret_from_env: bool,
    secret: String,
    dial_provider: String,
    base_url: String,
    sipgate: Vec<crate::config::FieldView>,
    sipgate_in_admin: bool,
    notice: String,
    calls: Vec<CallView>,
    agents: Vec<ExtensionRow>,
}

#[derive(Deserialize)]
struct NoticeQuery {
    notice: Option<String>,
}

async fn admin_page(State(st): State<AppState>, WebAdmin(me): WebAdmin, Query(q): Query<NoticeQuery>) -> WebResult<Html<String>> {
    let calls = telephony::recent(&st.db, 50).await?;
    let agents: Vec<(i64, String, Option<String>)> =
        sqlx::query_as("SELECT id, name, extension FROM users WHERE role IN ('agent', 'admin') AND active ORDER BY name")
            .fetch_all(&st.db)
            .await?;
    page(AdminPage {
        nav: Nav::new(&me, "admin"),
        tab: "telephony",
        secret_ok: telephony_secret().is_some(),
        secret_in_admin: crate::config::source("telephony.secret") == "admin",
        secret_from_env: crate::config::source("telephony.secret") == "env",
        secret: telephony_secret().unwrap_or_default(),
        dial_provider: telephony::dial_provider().unwrap_or("").to_string(),
        base_url: crate::email::public_url(),
        sipgate: crate::config::fields("Telefonie").into_iter().filter(|f| f.key.starts_with("sipgate.")).collect(),
        sipgate_in_admin: ["sipgate.token_id", "sipgate.token"].iter().any(|k| crate::config::source(k) == "admin"),
        notice: q.notice.unwrap_or_default(),
        calls: call_views(&st.db, &calls).await?,
        agents: agents
            .into_iter()
            .map(|(id, name, ext)| ExtensionRow { id, name, extension: ext.unwrap_or_default() })
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// Webhooks der Anbieter
// ---------------------------------------------------------------------------

fn telephony_secret() -> Option<String> {
    crate::config::opt("telephony.secret").filter(|s| s.len() >= 16)
}

async fn rotate_secret(State(st): State<AppState>, WebAdmin(_): WebAdmin) -> WebResult<Response> {
    crate::config::set(&st.db, "telephony.secret", Some(&crate::config::random_secret())).await?;
    Ok(see_other("/admin/telephony?notice=Neues+Webhook-Geheimnis+erzeugt"))
}

async fn clear_secret(State(st): State<AppState>, WebAdmin(_): WebAdmin) -> WebResult<Response> {
    crate::config::set(&st.db, "telephony.secret", None).await?;
    Ok(see_other("/admin/telephony?notice=Telefonie-Webhooks+deaktiviert"))
}

async fn save_sipgate(
    State(st): State<AppState>,
    WebAdmin(_): WebAdmin,
    Form(pairs): Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    if pairs.iter().any(|(k, _)| k == "remove") {
        for key in ["sipgate.token_id", "sipgate.token"] {
            crate::config::set(&st.db, key, None).await?;
        }
        return Ok(see_other("/admin/telephony?notice=sipgate-Zugang+entfernt"));
    }
    let pairs: Vec<(String, String)> = pairs.into_iter().filter(|(k, _)| k.starts_with("sipgate.")).collect();
    match crate::config::save_section(&st.db, &["Telefonie"], &pairs).await {
        Ok(()) => Ok(see_other("/admin/telephony?notice=sipgate-Zugang+gespeichert")),
        Err(AppError::Unprocessable(msg)) => Ok(see_other(&with_query("/admin/telephony", &[("notice", &msg)]))),
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct SecretQuery {
    secret: Option<String>,
}

fn check_secret(q: &SecretQuery) -> AppResult<()> {
    let Some(secret) = telephony_secret() else { return Err(AppError::NotFound) };
    let given = q.secret.as_deref().unwrap_or_default();
    let same = given.len() == secret.len() && given.bytes().zip(secret.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
    if same { Ok(()) } else { Err(AppError::Unauthorized) }
}

/// sipgate liefert Nummern international ohne „+“ (z. B. 4930123456).
fn sipgate_number(n: &str) -> String {
    if !n.is_empty() && !n.starts_with('0') && n.chars().all(|c| c.is_ascii_digit()) { format!("+{n}") } else { n.to_string() }
}

fn field<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
}

/// sipgate.io Push-API (Formular-POST: event=newCall|answer|hangup).
/// Auf newCall antworten wir mit XML, damit sipgate auch answer/hangup meldet.
async fn sipgate_webhook(
    State(st): State<AppState>,
    Query(q): Query<SecretQuery>,
    axum::Form(pairs): axum::Form<Vec<(String, String)>>,
) -> AppResult<Response> {
    check_secret(&q)?;
    let call_id = field(&pairs, "callId").ok_or_else(|| AppError::invalid("callId fehlt"))?;
    match field(&pairs, "event").unwrap_or("") {
        "newCall" => {
            let direction = if field(&pairs, "direction") == Some("out") { "outbound" } else { "inbound" };
            let agent = match field(&pairs, "userId[]").or_else(|| field(&pairs, "userId")) {
                Some(ident) if direction == "outbound" => telephony::find_agent(&st.db, ident).await?,
                _ => None,
            };
            telephony::call_started(
                &st.db,
                "sipgate",
                call_id,
                direction,
                &sipgate_number(field(&pairs, "from").unwrap_or("")),
                &sipgate_number(field(&pairs, "to").unwrap_or("")),
                agent.as_ref(),
            )
            .await?;
            let url = format!("{}/telephony/sipgate?secret={}", crate::email::public_url(), q.secret.unwrap_or_default());
            let xml = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Response onAnswer="{url}" onHangup="{url}"/>"#,
                url = url.replace('&', "&amp;")
            );
            Ok(([(header::CONTENT_TYPE, "application/xml")], xml).into_response())
        }
        "answer" => {
            let ident = field(&pairs, "userId").or_else(|| field(&pairs, "user")).unwrap_or("");
            let agent = telephony::find_agent(&st.db, ident).await?;
            telephony::call_answered(&st.db, "sipgate", call_id, agent.as_ref()).await?;
            Ok(StatusCode::OK.into_response())
        }
        "hangup" => {
            let cause = field(&pairs, "cause");
            telephony::call_ended(&st.db, "sipgate", call_id, cause, None, None).await?;
            Ok(StatusCode::OK.into_response())
        }
        _ => Ok(StatusCode::OK.into_response()),
    }
}

/// Twilio Status-Callback (Formular-POST mit CallSid/CallStatus).
async fn twilio_webhook(
    State(st): State<AppState>,
    Query(q): Query<SecretQuery>,
    axum::Form(pairs): axum::Form<Vec<(String, String)>>,
) -> AppResult<Response> {
    check_secret(&q)?;
    let sid = field(&pairs, "CallSid").ok_or_else(|| AppError::invalid("CallSid fehlt"))?;
    let direction = if field(&pairs, "Direction").unwrap_or("inbound").starts_with("outbound") { "outbound" } else { "inbound" };
    let (from, to) = (field(&pairs, "From").unwrap_or(""), field(&pairs, "To").unwrap_or(""));
    let agent = match field(&pairs, "agent") {
        Some(ident) => telephony::find_agent(&st.db, ident).await?,
        None => None,
    };
    telephony::call_started(&st.db, "twilio", sid, direction, from, to, agent.as_ref()).await?;
    match field(&pairs, "CallStatus").unwrap_or("") {
        "in-progress" => {
            telephony::call_answered(&st.db, "twilio", sid, agent.as_ref()).await?;
        }
        status @ ("completed" | "busy" | "no-answer" | "canceled" | "failed") => {
            let duration = field(&pairs, "CallDuration").and_then(|d| d.parse().ok());
            let cause = if status == "failed" { "failed" } else { status };
            if status == "completed" && duration.unwrap_or(0) > 0 {
                telephony::call_answered(&st.db, "twilio", sid, agent.as_ref()).await?;
            }
            telephony::call_ended(&st.db, "twilio", sid, Some(cause), duration, field(&pairs, "RecordingUrl")).await?;
        }
        _ => {}
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Allgemeiner Anschluss für beliebige Telefonanlagen (3CX, Asterisk, eigene Skripte).
#[derive(Deserialize)]
struct GenericEvent {
    event: String,
    call_id: String,
    #[serde(default = "inbound")]
    direction: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    agent: Option<String>,
    duration: Option<i32>,
    recording_url: Option<String>,
    cause: Option<String>,
}

fn inbound() -> String {
    "inbound".into()
}

async fn generic_webhook(
    State(st): State<AppState>,
    Query(q): Query<SecretQuery>,
    crate::error::api_extract::Json(e): crate::error::api_extract::Json<GenericEvent>,
) -> AppResult<Json<serde_json::Value>> {
    check_secret(&q)?;
    let agent = match e.agent.as_deref() {
        Some(ident) => telephony::find_agent(&st.db, ident).await?,
        None => None,
    };
    let call = match e.event.as_str() {
        "ringing" | "started" => telephony::call_started(&st.db, "generic", &e.call_id, &e.direction, &e.from, &e.to, agent.as_ref()).await?,
        "answered" => telephony::call_answered(&st.db, "generic", &e.call_id, agent.as_ref()).await?,
        "ended" => {
            telephony::call_ended(&st.db, "generic", &e.call_id, e.cause.as_deref(), e.duration, e.recording_url.as_deref()).await?
        }
        other => return Err(AppError::invalid(format!("Unbekanntes Ereignis: {other} (erlaubt: ringing, answered, ended)"))),
    };
    Ok(Json(json!({ "call": call })))
}
