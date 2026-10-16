//! Agenten-Oberfläche: serverseitig gerendert (askama) mit htmx.

mod admin;
mod chat;
mod custom_reports;
mod files;
mod integrations;
mod kb;
mod list_prefs;
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
        .merge(list_prefs::router())
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
    pub user_id: i64,
    pub name: String,
    pub initials: String,
    pub is_admin: bool,
    pub section: &'static str,
    pub availability: String,
    pub availability_opts: Vec<Opt>,
    /// Version der Oberfläche (ändert sich bei jedem Serverstart) – erzwingt nach
    /// Updates ein vollständiges Neuladen und frische Stylesheets/Skripte.
    pub version: &'static str,
}

/// Navigation der Verwaltung: Bereiche mit ihren Seiten (Reiter-Kennung, Bezeichnung, Adresse).
pub const ADMIN_NAV: [(&str, &[(&str, &str, &str)]); 5] = [
    ("Allgemein", &[("settings", "Einstellungen", "/admin/settings"), ("hours", "Geschäftszeiten", "/admin/hours")]),
    ("Team", &[("agents", "Mitarbeitende", "/admin/agents"), ("groups", "Gruppen", "/admin/groups"), ("skills", "Fähigkeiten", "/admin/skills")]),
    (
        "Tickets",
        &[
            ("fields", "Ticketfelder", "/admin/fields"),
            ("views", "Ansichten", "/admin/rules/views"),
            ("macros", "Makros", "/admin/rules/macros"),
            ("sla", "SLA", "/admin/rules/sla"),
        ],
    ),
    (
        "Automatisierung",
        &[
            ("triggers", "Trigger", "/admin/rules/triggers"),
            ("automations", "Automationen", "/admin/rules/automations"),
            ("webhooks", "Webhooks", "/admin/webhooks"),
        ],
    ),
    ("Kanäle", &[("email", "E-Mail", "/admin/email"), ("telephony", "Telefonie", "/admin/telephony")]),
];

/// Bereich der Verwaltung, zu dem ein Reiter gehört.
pub fn admin_group(tab: &str) -> &'static str {
    ADMIN_NAV.iter().find(|(_, pages)| pages.iter().any(|p| p.0 == tab)).map(|g| g.0).unwrap_or("Allgemein")
}

/// Bereiche für die Navigation: (Bezeichnung, Adresse der ersten Seite, aktiv).
pub fn admin_groups(tab: &str) -> Vec<(&'static str, &'static str, bool)> {
    let current = admin_group(tab);
    ADMIN_NAV.iter().map(|(g, pages)| (*g, pages[0].2, *g == current)).collect()
}

/// Seiten des aktuellen Bereichs: (Bezeichnung, Adresse, aktiv).
pub fn admin_pages(tab: &str) -> Vec<(&'static str, &'static str, bool)> {
    let current = admin_group(tab);
    ADMIN_NAV
        .iter()
        .filter(|(g, _)| *g == current)
        .flat_map(|(_, pages)| pages.iter().map(|(key, label, url)| (*label, *url, *key == tab)))
        .collect()
}

/// Kennung dieses Serverlaufs für Stylesheet-/Skript-Adressen.
pub fn asset_version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| format!("{:x}", Utc::now().timestamp()))
}

impl Nav {
    pub fn new(user: &User, section: &'static str) -> Self {
        Nav {
            user_id: user.id,
            name: user.name.clone(),
            initials: initials(&user.name),
            is_admin: user.is_admin(),
            section,
            availability: user.availability.clone(),
            availability_opts: enum_opts(&AVAILABILITIES, availability_label, Some(&user.availability), None),
            version: asset_version(),
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

/// Pfad mit Query-String aus den nicht leeren Parametern (z. B. für `return_to`).
pub fn with_query(base: &str, params: &[(&str, &str)]) -> String {
    let set: Vec<(&str, &str)> = params.iter().copied().filter(|(_, v)| !v.trim().is_empty()).collect();
    if set.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", serde_urlencoded::to_string(&set).unwrap_or_default())
    }
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
    /// Textausschnitt mit Treffern (nur in der Suche).
    pub snippet: String,
    /// Kanal-Schlüssel (für das Symbol) und Anfang der Beschreibung als Vorschau.
    pub channel: String,
    pub preview: String,
    /// Letzte öffentliche Antwort eines Agenten (relativ), leer = noch keine.
    pub last_reply: String,
    /// Letzte öffentliche Nachricht des Kunden (relativ).
    pub last_customer: String,
    pub due: String,
    pub solved: String,
    /// „gut“ / „schlecht“ oder leer.
    pub satisfaction: &'static str,
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
    pub assignee_id: i64,
    pub created: String,
    pub type_label: &'static str,
}

/// Einzeilige Textvorschau (ohne Formatierung, gekürzt).
pub fn preview_text(body: &str) -> String {
    let plain = crate::markdown::to_plain(body);
    let line = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 160 {
        format!("{} …", line.chars().take(160).collect::<String>().trim_end())
    } else {
        line
    }
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
    let ticket_ids: Vec<i64> = tickets.iter().map(|t| t.id).collect();
    #[allow(clippy::type_complexity)]
    let last: HashMap<i64, (Option<DateTime<Utc>>, Option<DateTime<Utc>>)> =
        sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
            "SELECT c.ticket_id,
                    max(c.created_at) FILTER (WHERE u.role <> 'end-user'),
                    max(c.created_at) FILTER (WHERE u.role = 'end-user')
             FROM ticket_comments c JOIN users u ON u.id = c.author_id
             WHERE c.public AND c.ticket_id = ANY($1)
             GROUP BY c.ticket_id",
        )
        .bind(&ticket_ids)
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|(id, agent, customer)| (id, (agent, customer)))
        .collect();
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
                snippet: String::new(),
                channel: t.channel.clone(),
                preview: preview_text(&t.description),
                last_reply: last.get(&t.id).and_then(|l| l.0).map(|d| fmt_rel(d, now)).unwrap_or_default(),
                last_customer: last.get(&t.id).and_then(|l| l.1).map(|d| fmt_rel(d, now)).unwrap_or_default(),
                due: t.due_at.map(fmt_dt).unwrap_or_default(),
                solved: t.solved_at.map(|d| fmt_rel(d, now)).unwrap_or_default(),
                satisfaction: match t.satisfaction_score.as_deref() {
                    Some("good") => "gut",
                    Some("bad") => "schlecht",
                    _ => "",
                },
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
                assignee_id: t.assignee_id.unwrap_or_default(),
                assignee_initials: t.assignee_id.and_then(|id| users.get(&id)).map(|n| initials(n)).unwrap_or_default(),
                created: fmt_rel(t.created_at, now),
                type_label: t.ticket_type.as_deref().map(type_label).unwrap_or(""),
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
