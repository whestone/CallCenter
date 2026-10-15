//! E-Mail in der Oberfläche: Passwort vergessen/neu setzen, Admin-Seite (Vorlagen,
//! Postausgang, Test-Mail) und der Webhook für eingehende E-Mails.

use askama::Template;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use super::extract::{Form, Path, Query, State};
use super::*;
use crate::{
    auth::WebAdmin,
    email::{self, InboundEmail, InboundResult},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/password/forgot", get(forgot_page).post(forgot))
        .route("/password/reset/{token}", get(reset_page).post(reset))
        .route("/admin/email", get(admin_page).post(save_templates))
        .route("/admin/email/test", post(send_test))
        .route("/admin/email/smtp", post(save_smtp))
        .route("/admin/email/smtp/delete", post(clear_smtp))
        .route("/api/v2/inbound/email", post(inbound))
}

// ---------------------------------------------------------------------------
// Passwort vergessen
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "password.html")]
struct PasswordPage {
    mode: &'static str,
    portal: bool,
    message: String,
    error: String,
    token: String,
    name: String,
}

#[derive(Deserialize)]
struct PortalFlag {
    portal: Option<String>,
}

async fn forgot_page(Query(q): Query<PortalFlag>) -> WebResult<Html<String>> {
    page(PasswordPage {
        mode: "forgot",
        portal: q.portal.is_some(),
        message: String::new(),
        error: String::new(),
        token: String::new(),
        name: String::new(),
    })
}

#[derive(Deserialize)]
struct ForgotForm {
    email: String,
    portal: Option<String>,
}

async fn forgot(State(st): State<AppState>, Form(f): Form<ForgotForm>) -> WebResult<Html<String>> {
    email::request_password_reset(&st.db, &f.email).await?;
    // Gleiche Antwort, egal ob die Adresse existiert (keine Kontenausspähung).
    page(PasswordPage {
        mode: "sent",
        portal: f.portal.is_some(),
        message: "Falls ein Konto mit dieser E-Mail-Adresse existiert, haben wir einen Link zum Zurücksetzen geschickt. Er ist eine Stunde gültig.".into(),
        error: String::new(),
        token: String::new(),
        name: String::new(),
    })
}

async fn reset_page(State(st): State<AppState>, Path(token): Path<String>) -> WebResult<Html<String>> {
    let user = email::reset_token_user(&st.db, &token).await?;
    page(PasswordPage {
        mode: if user.is_some() { "reset" } else { "invalid" },
        portal: user.as_ref().is_some_and(|u| !u.is_agent()),
        message: String::new(),
        error: String::new(),
        name: user.map(|u| u.name).unwrap_or_default(),
        token,
    })
}

#[derive(Deserialize)]
struct ResetForm {
    password: String,
    repeat: String,
}

async fn reset(State(st): State<AppState>, Path(token): Path<String>, Form(f): Form<ResetForm>) -> WebResult<Response> {
    let result = if f.password != f.repeat {
        Err(AppError::invalid("Die Passwörter stimmen nicht überein"))
    } else {
        email::complete_password_reset(&st.db, &token, &f.password).await
    };
    match result {
        Ok(user) => {
            let html = page(PasswordPage {
                mode: "done",
                portal: !user.is_agent(),
                message: "Ihr Passwort wurde geändert. Sie können sich jetzt anmelden.".into(),
                error: String::new(),
                token: String::new(),
                name: user.name,
            })?;
            Ok(html.into_response())
        }
        Err(AppError::Unprocessable(msg)) => {
            let user = email::reset_token_user(&st.db, &token).await?;
            let html = page(PasswordPage {
                mode: if user.is_some() { "reset" } else { "invalid" },
                portal: user.as_ref().is_some_and(|u| !u.is_agent()),
                message: String::new(),
                error: msg,
                name: user.map(|u| u.name).unwrap_or_default(),
                token,
            })?;
            Ok((StatusCode::UNPROCESSABLE_ENTITY, html).into_response())
        }
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------------------
// Admin: Vorlagen, Postausgang, Test
// ---------------------------------------------------------------------------

struct TemplateField {
    key: &'static str,
    label: &'static str,
    value: String,
    multiline: bool,
}

struct OutboxRow {
    ticket_id: i64,
    to: String,
    subject: String,
    status: String,
    status_label: &'static str,
    error: String,
    created: String,
}

#[derive(Template)]
#[template(path = "admin/email.html")]
struct EmailPage {
    nav: Nav,
    tab: &'static str,
    smtp: String,
    smtp_ok: bool,
    smtp_host: String,
    smtp_port: String,
    smtp_user: String,
    smtp_from: String,
    smtp_has_password: bool,
    smtp_in_admin: bool,
    security_opts: Vec<Opt>,
    inbound_url: String,
    inbound_enabled: bool,
    imap_ok: bool,
    imap: String,
    imap_status: String,
    imap_status_ok: bool,
    templates: Vec<TemplateField>,
    outbox: Vec<OutboxRow>,
    notice: String,
}

#[derive(Deserialize)]
struct NoticeQuery {
    notice: Option<String>,
}

async fn admin_page(State(st): State<AppState>, WebAdmin(me): WebAdmin, Query(q): Query<NoticeQuery>) -> WebResult<Html<String>> {
    let settings = email::settings(&st.db).await?;
    let now = chrono::Utc::now();
    let smtp = email::smtp_from(&settings);
    let get = |k: &str| settings.get(k).cloned().unwrap_or_default();
    let security = Some(get("smtp.security")).filter(|s| !s.is_empty()).unwrap_or_else(|| "starttls".into());
    page(EmailPage {
        nav: Nav::new(&me, "admin"),
        tab: "email",
        smtp_ok: smtp.is_some(),
        smtp: smtp
            .map(|c| {
                let source = if c.source == "env" { " · aus Umgebungsvariablen" } else { "" };
                format!("{}:{} · Absender {}{source}", c.host, c.port, c.from)
            })
            .unwrap_or_else(|| "nicht eingerichtet".into()),
        smtp_in_admin: !get("smtp.host").is_empty(),
        smtp_host: get("smtp.host"),
        smtp_port: get("smtp.port"),
        smtp_user: get("smtp.user"),
        smtp_from: get("smtp.from"),
        smtp_has_password: !get("smtp.password").is_empty(),
        security_opts: [("starttls", "STARTTLS"), ("tls", "SSL/TLS"), ("none", "keine")]
            .iter()
            .map(|(v, l)| Opt { value: v.to_string(), label: l.to_string(), selected: security == *v })
            .collect(),
        inbound_enabled: inbound_secret().is_some(),
        imap_ok: crate::imap::config().is_some(),
        imap: crate::imap::config()
            .map(|c| format!("{}@{}:{} ({}), Ordner {}, alle {} s", c.user, c.host, c.port, c.security, c.folder, c.interval))
            .unwrap_or_else(|| "IMAP_HOST, IMAP_USER und IMAP_PASSWORD setzen".into()),
        imap_status: crate::imap::last_status()
            .map(|s| format!("Letzter Abruf {}: {}", fmt_rel(s.at, chrono::Utc::now()), s.message))
            .unwrap_or_default(),
        imap_status_ok: crate::imap::last_status().is_none_or(|s| s.ok),
        inbound_url: format!("{}/api/v2/inbound/email?secret=…", email::public_url()),
        templates: email::TEMPLATE_KEYS
            .iter()
            .map(|(key, label)| TemplateField {
                key,
                label,
                value: settings.get(*key).cloned().unwrap_or_default(),
                multiline: key.ends_with("_body") || key.ends_with("signature"),
            })
            .collect(),
        outbox: email::outbox(&st.db, 50)
            .await?
            .into_iter()
            .map(|(_, ticket_id, to, subject, status, error, created)| OutboxRow {
                ticket_id: ticket_id.unwrap_or(0),
                to,
                subject,
                status_label: match status.as_str() {
                    "sent" => "gesendet",
                    "pending" => "wartet",
                    "failed" => "fehlgeschlagen",
                    _ => "nicht gesendet",
                },
                status,
                error: error.unwrap_or_default(),
                created: fmt_rel(created, now),
            })
            .collect(),
        notice: q.notice.unwrap_or_default(),
    })
}

async fn save_templates(
    State(st): State<AppState>,
    WebAdmin(_): WebAdmin,
    Form(pairs): Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    for (key, value) in &pairs {
        if email::TEMPLATE_KEYS.iter().any(|(k, _)| k == key) {
            email::save_setting(&st.db, key, value).await?;
        }
    }
    Ok(see_other("/admin/email?notice=Vorlagen+gespeichert"))
}

async fn save_smtp(
    State(st): State<AppState>,
    WebAdmin(_): WebAdmin,
    Form(pairs): Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    let values: std::collections::HashMap<&str, String> = pairs
        .iter()
        .filter_map(|(k, v)| email::SMTP_KEYS.iter().find(|key| **key == k.as_str()).map(|key| (*key, v.clone())))
        .collect();
    match email::save_smtp(&st.db, &values).await {
        Ok(()) => Ok(see_other("/admin/email?notice=SMTP-Zugang+gespeichert")),
        Err(AppError::Unprocessable(msg)) => Ok(see_other(&with_query("/admin/email", &[("notice", &msg)]))),
        Err(e) => Err(e.into()),
    }
}

async fn clear_smtp(State(st): State<AppState>, WebAdmin(_): WebAdmin) -> WebResult<Response> {
    email::clear_smtp(&st.db).await?;
    Ok(see_other("/admin/email?notice=SMTP-Zugang+entfernt"))
}

async fn send_test(State(st): State<AppState>, WebAdmin(me): WebAdmin) -> WebResult<Response> {
    let to = me.email.clone().unwrap_or_default();
    email::enqueue(&st.db, None, &to, "Test-E-Mail vom CallCenter", "Diese Test-E-Mail bestätigt, dass der Versand funktioniert.").await?;
    email::send_pending(&st.db).await?;
    Ok(see_other("/admin/email?notice=Test-E-Mail+in+den+Postausgang+gelegt"))
}

// ---------------------------------------------------------------------------
// Eingang
// ---------------------------------------------------------------------------

fn inbound_secret() -> Option<String> {
    std::env::var("INBOUND_EMAIL_SECRET").ok().filter(|s| s.trim().len() >= 16)
}

#[derive(Deserialize)]
struct InboundQuery {
    secret: Option<String>,
}

/// Webhook für eingehende E-Mails (z. B. von Mailgun/Postmark weitergeleitet).
/// Erwartet JSON `{ "from", "from_name", "subject", "text" }` und `?secret=…`.
async fn inbound(
    State(st): State<AppState>,
    Query(q): Query<InboundQuery>,
    Json(mail): Json<InboundEmail>,
) -> AppResult<(StatusCode, Json<serde_json::Value>)> {
    let Some(secret) = inbound_secret() else { return Err(AppError::NotFound) };
    if q.secret.as_deref() != Some(secret.as_str()) {
        return Err(AppError::Unauthorized);
    }
    Ok(match email::handle_inbound(&st.db, mail).await? {
        InboundResult::Reply(id) => (StatusCode::OK, Json(json!({ "result": "reply", "ticket_id": id }))),
        InboundResult::Created(id) => (StatusCode::CREATED, Json(json!({ "result": "created", "ticket_id": id }))),
    })
}
