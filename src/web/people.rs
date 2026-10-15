use askama::Template;
use axum::{
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Deserialize;

use super::extract::{Form, Path, Query, State};
use super::*;
use crate::{
    auth::WebAgent,
    service::{self, OrgInput, UserInput},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/users", get(users_index).post(create_user))
        .route("/users/{id}", get(show_user).post(update_user))
        .route("/users/{id}/merge", post(merge_user))
        .route("/users/{id}/export.json", get(export_user))
        .route("/users/{id}/anonymize", post(anonymize_user))
        .route("/organizations", get(orgs_index).post(create_org))
        .route("/organizations/{id}", get(show_org).post(update_org))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
}

// ---------------------------------------------------------------------------
// Kunden
// ---------------------------------------------------------------------------

struct UserRow {
    id: i64,
    name: String,
    email: String,
    phone: String,
    org: String,
}

#[derive(Template)]
#[template(path = "users.html")]
struct UsersPage {
    nav: Nav,
    q: String,
    users: Vec<UserRow>,
    org_opts: Vec<Opt>,
}

async fn user_rows(db: &PgPool, users: &[User]) -> AppResult<Vec<UserRow>> {
    let orgs: HashMap<i64, String> = sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM organizations")
        .fetch_all(db)
        .await?
        .into_iter()
        .collect();
    Ok(users
        .iter()
        .map(|u| UserRow {
            id: u.id,
            name: u.name.clone(),
            email: u.email.clone().unwrap_or_default(),
            phone: u.phone.clone().unwrap_or_default(),
            org: u.organization_id.and_then(|id| orgs.get(&id).cloned()).unwrap_or_default(),
        })
        .collect())
}

async fn org_opts(db: &PgPool, selected: Option<i64>) -> AppResult<Vec<Opt>> {
    let orgs: Vec<Organization> = sqlx::query_as("SELECT * FROM organizations ORDER BY name").fetch_all(db).await?;
    Ok(id_opts(orgs.iter().map(|o| (o.id, o.name.as_str())), selected, "– keine –"))
}

async fn users_index(State(st): State<AppState>, WebAgent(me): WebAgent, Query(q): Query<SearchQuery>) -> WebResult<Html<String>> {
    let query = q.q.unwrap_or_default();
    let users: Vec<User> = if query.trim().is_empty() {
        sqlx::query_as("SELECT * FROM users WHERE role = 'end-user' ORDER BY updated_at DESC LIMIT 100")
            .fetch_all(&st.db)
            .await?
    } else {
        service::search_users(&st.db, &query, 100, true).await?
    };
    page(UsersPage {
        nav: Nav::new(&me, "users"),
        q: query,
        users: user_rows(&st.db, &users).await?,
        org_opts: org_opts(&st.db, None).await?,
    })
}

#[derive(Deserialize)]
struct UserForm {
    name: Option<String>,
    email: Option<String>,
    phone: Option<String>,
    organization_id: Option<String>,
    /// Nur im Admin-Formular vorhanden: dann bedeutet ein fehlendes `active` "deaktiviert".
    active_field: Option<String>,
    active: Option<String>,
    portal_password: Option<String>,
}

impl UserForm {
    /// Kunden-Formular: Rollen werden hier nie geändert (Mitarbeitende → Verwaltung → Team).
    fn into_input(self, admin: bool) -> UserInput {
        UserInput {
            name: self.name,
            email: self.email,
            phone: self.phone,
            organization_id: Some(parse_id(&self.organization_id)),
            role: None,
            active: if admin && self.active_field.is_some() { Some(self.active.is_some()) } else { None },
            password: self.portal_password,
            availability: None,
            capacity: None,
        }
    }
}

async fn create_user(State(st): State<AppState>, WebAgent(me): WebAgent, Form(f): Form<UserForm>) -> WebResult<Response> {
    let mut input = f.into_input(false);
    // Leeres Feld = keine Vorgabe → Organisation über die E-Mail-Domain bestimmen.
    if input.organization_id == Some(None) {
        input.organization_id = None;
    }
    let user = service::create_user(&st.db, &me, input).await?;
    Ok(see_other(&format!("/users/{}", user.id)))
}

#[derive(Template)]
#[template(path = "user.html")]
struct UserPage {
    nav: Nav,
    id: i64,
    name: String,
    initials: String,
    email: String,
    phone: String,
    is_end_user: bool,
    can_edit: bool,
    can_admin: bool,
    active: bool,
    created: String,
    org_opts: Vec<Opt>,
    rows: Vec<TicketRow>,
    calls: Vec<super::telephony::CallView>,
}

async fn load_user(db: &PgPool, id: i64) -> AppResult<User> {
    sqlx::query_as("SELECT * FROM users WHERE id = $1").bind(id).fetch_optional(db).await?.ok_or(AppError::NotFound)
}

async fn show_user(State(st): State<AppState>, WebAgent(me): WebAgent, Path(id): Path<i64>) -> WebResult<Response> {
    let u = load_user(&st.db, id).await?;
    // Mitarbeitende werden ausschließlich unter Verwaltung → Team gepflegt.
    if u.is_agent() {
        return if me.is_admin() { Ok(see_other(&format!("/admin/agents/{id}"))) } else { Err(AppError::NotFound.into()) };
    }
    let tickets: Vec<Ticket> = sqlx::query_as(
        "SELECT * FROM tickets WHERE requester_id = $1 OR assignee_id = $1 ORDER BY updated_at DESC LIMIT 100",
    )
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    page(UserPage {
        nav: Nav::new(&me, "users"),
        id: u.id,
        initials: initials(&u.name),
        name: u.name.clone(),
        email: u.email.clone().unwrap_or_default(),
        phone: u.phone.clone().unwrap_or_default(),
        is_end_user: !u.is_agent(),
        can_edit: me.is_admin() || !u.is_agent() || me.id == u.id,
        can_admin: me.is_admin(),
        active: u.active,
        created: fmt_dt(u.created_at),
        org_opts: org_opts(&st.db, u.organization_id).await?,
        rows: ticket_rows(&st.db, &tickets).await?,
        calls: super::telephony::call_views(&st.db, &crate::telephony::for_customer(&st.db, id, 20).await?).await?,
    })
    .map(IntoResponse::into_response)
}

async fn update_user(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path(id): Path<i64>,
    Form(f): Form<UserForm>,
) -> WebResult<Response> {
    if load_user(&st.db, id).await?.is_agent() {
        return Err(AppError::invalid("Mitarbeitende werden unter Verwaltung → Team bearbeitet").into());
    }
    let admin = me.is_admin();
    service::update_user(&st.db, &me, id, f.into_input(admin)).await?;
    Ok(see_other_ok(&format!("/users/{id}"), "Gespeichert"))
}

#[derive(Deserialize)]
struct MergeForm {
    target: String,
}

/// Zielkunde per ID, E-Mail oder Telefonnummer.
async fn find_customer(db: &PgPool, needle: &str) -> AppResult<User> {
    let needle = needle.trim();
    let by_id: Option<User> = match needle.parse::<i64>() {
        Ok(id) if needle.len() < 10 => sqlx::query_as("SELECT * FROM users WHERE id = $1").bind(id).fetch_optional(db).await?,
        _ => None,
    };
    if let Some(u) = by_id {
        return Ok(u);
    }
    let phone = service::normalize_phone(needle);
    sqlx::query_as("SELECT * FROM users WHERE lower(email) = lower($1) OR ($2 <> '' AND phone = $2) ORDER BY id LIMIT 1")
        .bind(needle)
        .bind(phone)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::invalid(format!("Kein Kunde zu „{needle}“ gefunden")))
}

async fn merge_user(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path(id): Path<i64>,
    Form(f): Form<MergeForm>,
) -> WebResult<Response> {
    let target = find_customer(&st.db, &f.target).await?;
    let merged = service::merge_users(&st.db, &me, id, target.id).await?;
    Ok(see_other(&format!("/users/{}", merged.id)))
}

/// DSGVO-Auskunft als JSON-Download (nur Admins).
async fn export_user(State(st): State<AppState>, crate::auth::WebAdmin(_): crate::auth::WebAdmin, Path(id): Path<i64>) -> WebResult<Response> {
    use axum::response::IntoResponse;
    let data = service::export_user_data(&st.db, id).await?;
    let body = serde_json::to_string_pretty(&data).map_err(|e| AppError::Internal(e.into()))?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"kunde-{id}-datenauskunft.json\"")),
        ],
        body,
    )
        .into_response())
}

async fn anonymize_user(State(st): State<AppState>, crate::auth::WebAdmin(me): crate::auth::WebAdmin, Path(id): Path<i64>) -> WebResult<Response> {
    service::anonymize_user(&st.db, &me, id).await?;
    Ok(see_other_ok(&format!("/users/{id}"), "Zusammengeführt"))
}

// ---------------------------------------------------------------------------
// Organisationen
// ---------------------------------------------------------------------------

struct OrgRow {
    id: i64,
    name: String,
    domains: String,
    users: i64,
    open_tickets: i64,
}

#[derive(Template)]
#[template(path = "organizations.html")]
struct OrgsPage {
    nav: Nav,
    orgs: Vec<OrgRow>,
    can_admin: bool,
}

async fn orgs_index(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Html<String>> {
    let rows: Vec<(i64, String, Vec<String>, i64, i64)> = sqlx::query_as(
        "SELECT o.id, o.name, o.domain_names,
                (SELECT count(*) FROM users u WHERE u.organization_id = o.id),
                (SELECT count(*) FROM tickets t WHERE t.organization_id = o.id AND t.status IN ('new','open','pending','hold'))
         FROM organizations o ORDER BY o.name",
    )
    .fetch_all(&st.db)
    .await?;
    page(OrgsPage {
        nav: Nav::new(&me, "orgs"),
        orgs: rows
            .into_iter()
            .map(|(id, name, domains, users, open_tickets)| OrgRow { id, name, domains: domains.join(", "), users, open_tickets })
            .collect(),
        can_admin: me.is_admin(),
    })
}

#[derive(Deserialize)]
struct OrgForm {
    name: Option<String>,
    domain_names: Option<String>,
    notes: Option<String>,
}

impl OrgForm {
    fn into_input(self) -> OrgInput {
        OrgInput {
            name: self.name,
            domain_names: self
                .domain_names
                .map(|d| d.split([',', ' ', '\n']).map(str::to_string).collect()),
            notes: self.notes,
        }
    }
}

async fn create_org(State(st): State<AppState>, WebAgent(me): WebAgent, Form(f): Form<OrgForm>) -> WebResult<Response> {
    if !me.is_admin() {
        return Err(AppError::Forbidden.into());
    }
    let org = service::create_org(&st.db, f.into_input()).await?;
    Ok(see_other(&format!("/organizations/{}", org.id)))
}

#[derive(Template)]
#[template(path = "organization.html")]
struct OrgPage {
    nav: Nav,
    id: i64,
    name: String,
    domains: String,
    notes: String,
    can_admin: bool,
    users: Vec<UserRow>,
    rows: Vec<TicketRow>,
}

async fn show_org(State(st): State<AppState>, WebAgent(me): WebAgent, Path(id): Path<i64>) -> WebResult<Html<String>> {
    let org: Organization = sqlx::query_as("SELECT * FROM organizations WHERE id = $1")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let users: Vec<User> = sqlx::query_as("SELECT * FROM users WHERE organization_id = $1 AND role = 'end-user' ORDER BY name")
        .bind(id)
        .fetch_all(&st.db)
        .await?;
    let tickets: Vec<Ticket> =
        sqlx::query_as("SELECT * FROM tickets WHERE organization_id = $1 ORDER BY updated_at DESC LIMIT 100")
            .bind(id)
            .fetch_all(&st.db)
            .await?;
    page(OrgPage {
        nav: Nav::new(&me, "orgs"),
        id: org.id,
        name: org.name,
        domains: org.domain_names.join(", "),
        notes: org.notes,
        can_admin: me.is_admin(),
        users: user_rows(&st.db, &users).await?,
        rows: ticket_rows(&st.db, &tickets).await?,
    })
}

async fn update_org(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path(id): Path<i64>,
    Form(f): Form<OrgForm>,
) -> WebResult<Response> {
    if !me.is_admin() {
        return Err(AppError::Forbidden.into());
    }
    service::update_org(&st.db, id, f.into_input()).await?;
    Ok(see_other_ok(&format!("/organizations/{id}"), "Gespeichert"))
}

