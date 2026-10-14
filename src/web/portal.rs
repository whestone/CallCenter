//! Kundenportal: Kunden melden sich an, legen Anfragen an, sehen den Verlauf
//! (nur öffentliche Antworten), antworten und markieren Anfragen als gelöst.

use askama::Template;
use axum::{
    http::{header, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::json;

use super::extract::{Form, Path, State};
use super::*;
use crate::{
    auth::{self, PortalUser},
    service::{self, CommentChange, NewTicket, TicketChanges, UserInput},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/portal", get(my_tickets))
        .route("/portal/login", get(login_page).post(login))
        .route("/portal/login/demo", post(demo_login))
        .route("/portal/register", post(register))
        .route("/portal/logout", post(logout))
        .route("/portal/tickets/new", get(new_page).post(create))
        .route("/portal/tickets/{id}", get(show).post(reply))
        .route("/portal/tickets/{id}/solve", post(solve))
        .route("/portal/tickets/{id}/rate", post(rate))
}

/// Status aus Kundensicht.
fn customer_status(s: &str) -> &'static str {
    match s {
        "new" | "open" => "Offen",
        "pending" => "Wartet auf Ihre Antwort",
        "hold" => "In Bearbeitung",
        "solved" => "Gelöst",
        "closed" => "Geschlossen",
        _ => "?",
    }
}

struct PortalNav {
    name: String,
}

#[derive(Template)]
#[template(path = "portal/login.html")]
struct LoginPage {
    error: String,
    email: String,
    mode: &'static str,
    name: String,
    demo: bool,
}

async fn login_page() -> WebResult<Html<String>> {
    page(LoginPage { error: String::new(), email: String::new(), mode: "login", name: String::new(), demo: demo_login_enabled() })
}

#[derive(Deserialize)]
struct LoginForm {
    email: String,
    password: String,
}

fn with_session(token: &str, to: &str) -> Response {
    ([(header::SET_COOKIE, auth::session_cookie(token))], Redirect::to(to)).into_response()
}

async fn login(State(st): State<AppState>, Form(f): Form<LoginForm>) -> WebResult<Response> {
    match auth::authenticate_any(&st.db, &f.email, &f.password).await? {
        Some(user) => Ok(with_session(&auth::create_session(&st.db, user.id).await?, "/portal")),
        None => {
            let html = page(LoginPage {
                error: "E-Mail oder Passwort ist falsch.".into(),
                email: f.email,
                mode: "login",
                name: String::new(),
                demo: demo_login_enabled(),
            })?;
            Ok((StatusCode::UNAUTHORIZED, html).into_response())
        }
    }
}

async fn demo_login(State(st): State<AppState>) -> WebResult<Response> {
    if !demo_login_enabled() {
        return Err(AppError::NotFound.into());
    }
    let email = demo_email("customer").unwrap_or_default();
    let user: User = sqlx::query_as("SELECT * FROM users WHERE lower(email) = lower($1) AND active")
        .bind(&email)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| AppError::invalid(format!("Demo-Kunde {email} existiert nicht")))?;
    Ok(with_session(&auth::create_session(&st.db, user.id).await?, "/portal"))
}

#[derive(Deserialize)]
struct RegisterForm {
    name: String,
    email: String,
    password: String,
}

async fn register(State(st): State<AppState>, Form(f): Form<RegisterForm>) -> WebResult<Response> {
    let fail = |msg: &str, f: &RegisterForm| -> WebResult<Response> {
        let html = page(LoginPage { error: msg.into(), email: f.email.clone(), mode: "register", name: f.name.clone(), demo: demo_login_enabled() })?;
        Ok((StatusCode::UNPROCESSABLE_ENTITY, html).into_response())
    };
    if f.name.trim().is_empty() || !f.email.contains('@') {
        return fail("Bitte Name und eine gültige E-Mail-Adresse angeben.", &f);
    }
    if f.password.chars().count() < 8 {
        return fail("Das Passwort muss mindestens 8 Zeichen haben.", &f);
    }
    let existing: Option<User> = sqlx::query_as("SELECT * FROM users WHERE lower(email) = lower($1)")
        .bind(f.email.trim())
        .fetch_optional(&st.db)
        .await?;
    if let Some(u) = existing {
        // Bestehende Kundenkonten nicht ohne E-Mail-Bestätigung übernehmen lassen.
        return if u.password_hash.is_some() {
            fail("Für diese E-Mail gibt es bereits einen Zugang. Bitte melden Sie sich an.", &f)
        } else {
            fail("Zu dieser E-Mail gibt es bereits ein Kundenkonto. Bitte wenden Sie sich an den Support, um einen Portalzugang zu erhalten.", &f)
        };
    }
    // Systemnutzer für die Anlage: Selbstregistrierung erzeugt immer einen Endkunden.
    let creator = User {
        id: 0,
        name: "Portal".into(),
        email: None,
        phone: None,
        role: "end-user".into(),
        organization_id: None,
        password_hash: None,
        active: true,
        availability: "offline".into(),
        capacity: 0,
        totp_secret: None,
        totp_pending_secret: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let created = service::create_user(
        &st.db,
        &creator,
        UserInput {
            name: Some(f.name.clone()),
            email: Some(f.email.clone()),
            password: Some(f.password.clone()),
            ..Default::default()
        },
    )
    .await;
    match created {
        Ok(user) => Ok(with_session(&auth::create_session(&st.db, user.id).await?, "/portal")),
        Err(AppError::Unprocessable(msg)) => fail(&msg, &f),
        Err(e) => Err(e.into()),
    }
}

async fn logout(State(st): State<AppState>, headers: axum::http::HeaderMap) -> WebResult<Response> {
    if let Some(token) = auth::cookie(&headers, auth::COOKIE) {
        auth::delete_session(&st.db, &token).await?;
    }
    Ok(([(header::SET_COOKIE, auth::clear_cookie())], Redirect::to("/portal/login")).into_response())
}

struct PortalRow {
    id: i64,
    subject: String,
    status: String,
    status_label: &'static str,
    updated: String,
}

#[derive(Template)]
#[template(path = "portal/list.html")]
struct ListPage {
    nav: PortalNav,
    rows: Vec<PortalRow>,
}

async fn my_tickets(State(st): State<AppState>, PortalUser(me): PortalUser) -> WebResult<Html<String>> {
    let tickets: Vec<Ticket> = sqlx::query_as("SELECT * FROM tickets WHERE requester_id = $1 ORDER BY updated_at DESC")
        .bind(me.id)
        .fetch_all(&st.db)
        .await?;
    let now = chrono::Utc::now();
    page(ListPage {
        nav: PortalNav { name: me.name.clone() },
        rows: tickets
            .iter()
            .map(|t| PortalRow {
                id: t.id,
                subject: t.subject.clone(),
                status: t.status.clone(),
                status_label: customer_status(&t.status),
                updated: fmt_rel(t.updated_at, now),
            })
            .collect(),
    })
}

#[derive(Template)]
#[template(path = "portal/new.html")]
struct NewPage {
    nav: PortalNav,
}

async fn new_page(PortalUser(me): PortalUser) -> WebResult<Html<String>> {
    page(NewPage { nav: PortalNav { name: me.name.clone() } })
}

#[derive(Deserialize)]
struct NewForm {
    subject: String,
    description: String,
}

async fn create(State(st): State<AppState>, PortalUser(me): PortalUser, Form(f): Form<NewForm>) -> WebResult<Response> {
    let t = service::create_ticket(
        &st.db,
        &me,
        NewTicket {
            subject: f.subject,
            description: f.description,
            public: true,
            requester_id: me.id,
            channel: "web".into(),
            custom_fields: json!({}),
            ..Default::default()
        },
        "portal",
    )
    .await?;
    Ok(see_other(&format!("/portal/tickets/{}", t.id)))
}

struct PortalComment {
    author: String,
    initials: String,
    from_support: bool,
    body: String,
    created: String,
    attachments: Vec<super::files::AttachmentView>,
}

#[derive(Template)]
#[template(path = "portal/ticket.html")]
struct TicketPage {
    nav: PortalNav,
    id: i64,
    subject: String,
    status: String,
    status_label: &'static str,
    created: String,
    can_reply: bool,
    can_solve: bool,
    can_rate: bool,
    rating: String,
    rating_comment: String,
    comments: Vec<PortalComment>,
}

/// Lädt ein Ticket nur, wenn es dem angemeldeten Kunden gehört (sonst 404).
async fn own_ticket(db: &PgPool, me: &User, id: i64) -> AppResult<Ticket> {
    let t = service::get_ticket(db, id).await?;
    if t.requester_id != me.id {
        return Err(AppError::NotFound);
    }
    Ok(t)
}

async fn show(State(st): State<AppState>, PortalUser(me): PortalUser, Path(id): Path<i64>) -> WebResult<Html<String>> {
    let t = own_ticket(&st.db, &me, id).await?;
    let attachments = super::files::attachments_by_comment(&st.db, id).await?;
    let comments: Vec<(i64, String, String, String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT c.id, u.name, u.role, c.body, c.created_at FROM ticket_comments c JOIN users u ON u.id = c.author_id
         WHERE c.ticket_id = $1 AND c.public ORDER BY c.created_at, c.id",
    )
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    page(TicketPage {
        nav: PortalNav { name: me.name.clone() },
        id: t.id,
        subject: t.subject.clone(),
        status: t.status.clone(),
        status_label: customer_status(&t.status),
        created: fmt_dt(t.created_at),
        can_reply: t.status != "closed",
        can_solve: !matches!(t.status.as_str(), "solved" | "closed"),
        can_rate: matches!(t.status.as_str(), "solved" | "closed"),
        rating: t.satisfaction_score.clone().unwrap_or_default(),
        rating_comment: t.satisfaction_comment.clone().unwrap_or_default(),
        comments: comments
            .into_iter()
            .map(|(cid, author, role, body, created)| PortalComment {
                initials: initials(&author),
                from_support: role != "end-user",
                author,
                body,
                created: fmt_dt(created),
                attachments: attachments.get(&cid).map(|l| l.iter().map(Into::into).collect()).unwrap_or_default(),
            })
            .collect(),
    })
}

#[derive(Deserialize)]
struct ReplyForm {
    comment: String,
}

async fn reply(
    State(st): State<AppState>,
    PortalUser(me): PortalUser,
    Path(id): Path<i64>,
    Form(f): Form<ReplyForm>,
) -> WebResult<Response> {
    own_ticket(&st.db, &me, id).await?;
    if f.comment.trim().is_empty() {
        return Err(AppError::invalid("Bitte geben Sie eine Nachricht ein").into());
    }
    let changes = TicketChanges {
        comment: Some(CommentChange { body: f.comment, public: true, author_id: None }),
        ..Default::default()
    };
    service::update_ticket(&st.db, &me, id, changes, None, "portal").await?;
    Ok(see_other(&format!("/portal/tickets/{id}")))
}

#[derive(Deserialize)]
struct RateForm {
    score: String,
    #[serde(default)]
    comment: String,
}

async fn rate(
    State(st): State<AppState>,
    PortalUser(me): PortalUser,
    Path(id): Path<i64>,
    Form(f): Form<RateForm>,
) -> WebResult<Response> {
    service::rate_ticket(&st.db, &me, id, &f.score, &f.comment).await?;
    Ok(see_other(&format!("/portal/tickets/{id}")))
}

async fn solve(State(st): State<AppState>, PortalUser(me): PortalUser, Path(id): Path<i64>) -> WebResult<Response> {
    own_ticket(&st.db, &me, id).await?;
    let changes = TicketChanges { status: Some("solved".into()), ..Default::default() };
    service::update_ticket(&st.db, &me, id, changes, None, "portal").await?;
    Ok(see_other(&format!("/portal/tickets/{id}")))
}
