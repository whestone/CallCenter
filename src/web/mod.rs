//! Agenten-Oberfläche: serverseitig gerendert (askama) mit htmx.

mod admin;
mod chat;
mod custom_reports;
mod files;
mod integrations;
mod kb;
mod lists;
mod mail;
mod notifications;
mod people;
mod portal;
mod profile;
mod reports;
mod search;
mod telephony;
mod tickets;

use std::collections::HashMap;

use askama::Template;
use axum::{
    http::{header, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use chrono::{DateTime, Local, Utc};
use serde::Deserialize;
use sqlx::PgPool;

use self::extract::{Form, State};
use crate::{
    auth,
    error::{AppError, AppResult},
    models::*,
    sla, AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(|| async { Redirect::to("/views") }))
        .route("/login", get(login_page).post(login))
        .route("/login/demo", post(demo_login))
        .route("/login/2fa", post(login_2fa))
        .route("/logout", post(logout))
        .route("/me/availability", post(set_availability))
        .route("/hilfe", get(help_page))
        .merge(lists::router())
        .merge(tickets::router())
        .merge(people::router())
        .merge(reports::router())
        .merge(custom_reports::router())
        .merge(search::router())
        .merge(portal::router())
        .merge(files::router())
        .merge(profile::router())
        .merge(mail::router())
        .merge(kb::router())
        .merge(integrations::router())
        .merge(telephony::router())
        .merge(notifications::router())
        .merge(chat::router())
        .merge(admin::personal_router())
        .merge(admin::router())
}

// ---------------------------------------------------------------------------
// Fehler & Rendering
// ---------------------------------------------------------------------------

/// Fehler der Web-Oberfläche: wird als HTML-Seite statt JSON ausgegeben.
pub struct WebError(pub AppError);

impl<E: Into<AppError>> From<E> for WebError {
    fn from(e: E) -> Self {
        WebError(e.into())
    }
}

pub type WebResult<T> = Result<T, WebError>;

/// Extractoren, deren Ablehnungen als HTML-Fehlerseite statt rohem Text erscheinen.
pub mod extract {
    use axum::extract::{FromRequest, FromRequestParts};

    use super::WebError;

    pub use axum::extract::State;

    #[derive(FromRequestParts)]
    #[from_request(via(axum::extract::Path), rejection(WebError))]
    pub struct Path<T>(pub T);

    #[derive(FromRequestParts)]
    #[from_request(via(axum::extract::Query), rejection(WebError))]
    pub struct Query<T>(pub T);

    #[derive(FromRequest)]
    #[from_request(via(axum::Form), rejection(WebError))]
    pub struct Form<T>(pub T);
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        if matches!(self.0, AppError::Unauthorized) {
            return Redirect::to("/login").into_response();
        }
        let status = self.0.status();
        let page = ErrorPage { code: status.as_u16(), message: self.0.public_message() };
        let body = page.render().unwrap_or_else(|_| "Fehler".into());
        (status, Html(body)).into_response()
    }
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage {
    code: u16,
    message: String,
}

pub fn page<T: Template>(t: T) -> WebResult<Html<String>> {
    Ok(Html(t.render().map_err(AppError::from)?))
}

pub fn see_other(path: &str) -> Response {
    Redirect::to(path).into_response()
}

/// Weiterleitung mit kurzer Bestätigung („✓ Gespeichert“), die app.js anzeigt.
pub fn see_other_ok(path: &str, message: &str) -> Response {
    let sep = if path.contains('?') { '&' } else { '?' };
    let ok = serde_urlencoded::to_string([("ok", message)]).unwrap_or_default();
    see_other(&format!("{path}{sep}{ok}"))
}

// ---------------------------------------------------------------------------
// Gemeinsame View-Modelle
// ---------------------------------------------------------------------------

pub struct Nav {
    pub name: String,
    pub initials: String,
    pub is_admin: bool,
    pub section: &'static str,
    pub availability: String,
    pub availability_opts: Vec<Opt>,
}

impl Nav {
    pub fn new(user: &User, section: &'static str) -> Self {
        Nav {
            name: user.name.clone(),
            initials: initials(&user.name),
            is_admin: user.is_admin(),
            section,
            availability: user.availability.clone(),
            availability_opts: enum_opts(&AVAILABILITIES, availability_label, Some(&user.availability), None),
        }
    }
}

pub struct Opt {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

pub fn enum_opts(values: &[&str], label: fn(&str) -> &'static str, selected: Option<&str>, blank: Option<&str>) -> Vec<Opt> {
    let mut out = Vec::new();
    if let Some(b) = blank {
        out.push(Opt { value: String::new(), label: b.into(), selected: selected.is_none() });
    }
    out.extend(values.iter().map(|v| Opt {
        value: (*v).into(),
        label: label(v).into(),
        selected: selected == Some(*v),
    }));
    out
}

pub fn id_opts<'a>(items: impl IntoIterator<Item = (i64, &'a str)>, selected: Option<i64>, blank: &str) -> Vec<Opt> {
    let mut out = vec![Opt { value: String::new(), label: blank.into(), selected: selected.is_none() }];
    out.extend(items.into_iter().map(|(id, name)| Opt {
        value: id.to_string(),
        label: name.into(),
        selected: selected == Some(id),
    }));
    out
}

pub fn initials(name: &str) -> String {
    name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase()
}

pub fn fmt_dt(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&Local).format("%d.%m.%Y %H:%M").to_string()
}

pub fn fmt_rel(dt: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let mins = (now - dt).num_minutes();
    match mins {
        m if m < 1 => "gerade eben".into(),
        m if m < 60 => format!("vor {m} Min"),
        m if m < 60 * 24 => format!("vor {} Std", m / 60),
        m if m < 60 * 24 * 7 => format!("vor {} T", m / (60 * 24)),
        _ => fmt_dt(dt),
    }
}

/// Leere Formularfelder als "nicht gesetzt" behandeln.
pub fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

pub fn parse_id(s: &Option<String>) -> Option<i64> {
    non_empty(s).and_then(|s| s.parse().ok())
}

pub async fn user_names(db: &PgPool, ids: Vec<i64>) -> AppResult<HashMap<i64, String>> {
    let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM users WHERE id = ANY($1)")
        .bind(ids)
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().collect())
}

pub async fn group_names(db: &PgPool) -> AppResult<HashMap<i64, String>> {
    let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM agent_groups").fetch_all(db).await?;
    Ok(rows.into_iter().collect())
}

pub struct TicketRow {
    pub id: i64,
    pub subject: String,
    /// Betreff als HTML (maskiert; in der Suche mit hervorgehobenen Treffern).
    pub subject_html: String,
    pub status: String,
    pub status_label: &'static str,
    pub priority: String,
    pub priority_label: &'static str,
    pub requester: String,
    pub assignee: String,
    pub group: String,
    pub channel_label: &'static str,
    pub updated: String,
    pub sla_text: String,
    pub sla_class: &'static str,
    pub org: String,
    pub tags: Vec<String>,
    pub assignee_initials: String,
}

pub async fn ticket_rows(db: &PgPool, tickets: &[Ticket]) -> AppResult<Vec<TicketRow>> {
    let now = Utc::now();
    let ids = tickets.iter().flat_map(|t| [Some(t.requester_id), t.assignee_id]).flatten().collect();
    let users = user_names(db, ids).await?;
    let groups = group_names(db).await?;
    let orgs: HashMap<i64, String> = sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM organizations")
        .fetch_all(db)
        .await?
        .into_iter()
        .collect();
    let policies: HashMap<i64, SlaPolicy> =
        sla::load_policies_with_schedule(db).await?.into_iter().map(|p| (p.id, p)).collect();
    Ok(tickets
        .iter()
        .map(|t| {
            let next = t
                .sla_policy_id
                .and_then(|id| policies.get(&id))
                .and_then(|p| sla::next_target(p, t, now));
            let (sla_text, sla_class) = match next {
                Some(s) if s.breached => (sla::fmt_minutes(s.minutes_left), "sla breached"),
                Some(s) if s.minutes_left < 60 => (sla::fmt_minutes(s.minutes_left), "sla warn"),
                Some(s) => (sla::fmt_minutes(s.minutes_left), "sla ok"),
                None => (String::new(), ""),
            };
            TicketRow {
                id: t.id,
                subject_html: crate::markdown::escape(&t.subject),
                subject: t.subject.clone(),
                status: t.status.clone(),
                status_label: status_label(&t.status),
                priority: t.priority.clone().unwrap_or_default(),
                priority_label: t.priority.as_deref().map(priority_label).unwrap_or(""),
                requester: users.get(&t.requester_id).cloned().unwrap_or_default(),
                assignee: t.assignee_id.and_then(|id| users.get(&id).cloned()).unwrap_or_else(|| "–".into()),
                group: t.group_id.and_then(|id| groups.get(&id).cloned()).unwrap_or_else(|| "–".into()),
                channel_label: channel_label(&t.channel),
                updated: fmt_rel(t.updated_at, now),
                sla_text,
                sla_class,
                org: t.organization_id.and_then(|id| orgs.get(&id).cloned()).unwrap_or_default(),
                tags: t.tags.clone(),
                assignee_initials: t.assignee_id.and_then(|id| users.get(&id)).map(|n| initials(n)).unwrap_or_default(),
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    error: String,
    email: String,
    demo: bool,
    /// Gesetzt im zweiten Schritt (Code aus der Authenticator-App).
    challenge: String,
}

impl LoginPage {
    fn new(error: &str, email: &str) -> Self {
        LoginPage { error: error.into(), email: email.into(), demo: demo_login_enabled(), challenge: String::new() }
    }
}

/// Session anlegen, Agent online setzen, zur Arbeitsansicht.
async fn start_session(db: &PgPool, user: &User) -> WebResult<Response> {
    let token = auth::create_session(db, user.id).await?;
    // Wer sich anmeldet, ist erreichbar und bekommt Tickets per Routing.
    crate::service::set_availability(db, user.id, "online").await?;
    Ok(([(header::SET_COOKIE, auth::session_cookie(&token))], Redirect::to("/views")).into_response())
}

/// Demo-Anmeldung per Knopfdruck: nur wenn `DEMO_LOGIN=true` gesetzt ist.
pub fn demo_login_enabled() -> bool {
    std::env::var("DEMO_LOGIN").is_ok_and(|v| matches!(v.trim(), "true" | "1" | "yes"))
}

pub fn demo_email(role: &str) -> Option<String> {
    let (key, default) = match role {
        "admin" => ("SEED_ADMIN_EMAIL", "admin@example.com"),
        "agent" => ("SEED_AGENT_EMAIL", "agent@example.com"),
        "customer" => ("SEED_CUSTOMER_EMAIL", "max@acme.example"),
        _ => return None,
    };
    Some(std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.into()))
}

async fn login_page() -> WebResult<Html<String>> {
    page(LoginPage::new("", ""))
}

#[derive(Deserialize)]
struct DemoLoginForm {
    role: String,
}

async fn demo_login(State(st): State<AppState>, Form(f): Form<DemoLoginForm>) -> WebResult<Response> {
    if !demo_login_enabled() || !matches!(f.role.as_str(), "admin" | "agent") {
        return Err(AppError::NotFound.into());
    }
    let email = demo_email(&f.role).unwrap_or_default();
    let user: User = sqlx::query_as(
        "SELECT * FROM users WHERE lower(email) = lower($1) AND active AND role IN ('agent', 'admin')",
    )
    .bind(&email)
    .fetch_optional(&st.db)
    .await?
    .ok_or_else(|| AppError::invalid(format!("Demo-Konto {email} existiert nicht")))?;
    // Demo-Login ist nur für lokale Demos gedacht und überspringt bewusst die 2FA.
    start_session(&st.db, &user).await
}

#[derive(Deserialize)]
struct LoginForm {
    email: String,
    password: String,
}

async fn login(State(st): State<AppState>, Form(f): Form<LoginForm>) -> WebResult<Response> {
    match auth::authenticate(&st.db, &f.email, &f.password).await? {
        // Mit 2FA: erst den Code aus der Authenticator-App abfragen.
        Some(user) if user.totp_secret.is_some() => {
            let challenge = crate::twofactor::create_challenge(&st.db, user.id).await?;
            page(LoginPage { challenge, ..LoginPage::new("", &f.email) }).map(IntoResponse::into_response)
        }
        Some(user) => start_session(&st.db, &user).await,
        None => {
            let html = page(LoginPage::new("E-Mail oder Passwort ist falsch.", &f.email))?;
            Ok((StatusCode::UNAUTHORIZED, html).into_response())
        }
    }
}

#[derive(Deserialize)]
struct TwoFactorForm {
    challenge: String,
    code: String,
}

async fn login_2fa(State(st): State<AppState>, Form(f): Form<TwoFactorForm>) -> WebResult<Response> {
    use crate::twofactor::{complete_challenge, ChallengeResult};
    match complete_challenge(&st.db, &f.challenge, &f.code).await? {
        ChallengeResult::Ok(user) => start_session(&st.db, &user).await,
        ChallengeResult::WrongCode => {
            let html = page(LoginPage { challenge: f.challenge, ..LoginPage::new("Der Code ist falsch. Bitte den aktuellen Code aus der App eingeben.", "") })?;
            Ok((StatusCode::UNAUTHORIZED, html).into_response())
        }
        ChallengeResult::Expired => {
            let html = page(LoginPage::new("Die Anmeldung ist abgelaufen oder es gab zu viele Fehlversuche. Bitte erneut anmelden.", ""))?;
            Ok((StatusCode::UNAUTHORIZED, html).into_response())
        }
    }
}

async fn logout(State(st): State<AppState>, headers: axum::http::HeaderMap) -> WebResult<Response> {
    if let Some(token) = auth::cookie(&headers, auth::COOKIE) {
        if let Some(user_id) = auth::delete_session(&st.db, &token).await? {
            crate::service::set_availability(&st.db, user_id, "offline").await?;
        }
    }
    Ok(([(header::SET_COOKIE, auth::clear_cookie())], Redirect::to("/login")).into_response())
}

#[derive(Deserialize)]
struct AvailabilityForm {
    availability: String,
}

async fn set_availability(
    State(st): State<AppState>,
    auth::WebAgent(me): auth::WebAgent,
    headers: axum::http::HeaderMap,
    Form(f): Form<AvailabilityForm>,
) -> WebResult<Response> {
    crate::service::set_availability(&st.db, me.id, &f.availability).await?;
    let back = headers
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.split_once("://").and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_string())))
        .unwrap_or_else(|| "/views".into());
    Ok(see_other(&back))
}

#[derive(Template)]
#[template(path = "help.html")]
struct HelpPage {
    nav: Nav,
    base_url: String,
}

/// Hilfe für Agenten: Erklärungen gehören hierher statt in die Arbeitsoberfläche.
async fn help_page(crate::auth::WebAgent(me): crate::auth::WebAgent) -> WebResult<Html<String>> {
    page(HelpPage { nav: Nav::new(&me, "help"), base_url: crate::email::public_url() })
}
