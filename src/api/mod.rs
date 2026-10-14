//! REST-API im Stil der Zendesk Support API (`/api/v2/...`).
//!
//! Authentifizierung: HTTP Basic mit E-Mail und Passwort eines Agenten oder
//! das Session-Cookie der Web-Oberfläche.

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    auth::ApiAgent,
    error::{api_extract::{Json as JsonBody, Path, Query}, AppError, AppResult},
    models::*,
    service::{self, CommentChange, NewTicket, OrgInput, TicketChanges, UserInput},
    sla, AppState,
};

type JsonResult = AppResult<Json<Value>>;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/tickets", get(list_tickets).post(create_ticket))
        .route("/tickets/{id}", get(show_ticket).put(update_ticket).delete(delete_ticket))
        .route("/tickets/{id}/comments", get(ticket_comments))
        .route("/tickets/{id}/audits", get(ticket_audits))
        .route("/tickets/{id}/sla", get(ticket_sla))
        .route("/tickets/{id}/macros/{macro_id}/apply", post(apply_macro))
        .route("/users", get(list_users).post(create_user))
        .route("/users/me", get(me))
        .route("/users/search", get(search_users))
        .route("/users/{id}", get(show_user).put(update_user))
        .route("/users/{id}/tickets/requested", get(user_tickets))
        .route("/organizations", get(list_orgs).post(create_org))
        .route("/organizations/{id}", get(show_org).put(update_org))
        .route("/groups", get(list_groups).post(create_group))
        .route("/views", get(list_views))
        .route("/views/{id}/tickets", get(view_tickets))
        .route("/macros", get(list_macros))
        .route("/triggers", get(list_triggers))
        .route("/slas/policies", get(list_sla_policies))
        .route("/search", get(search))
}

// ---------------------------------------------------------------------------
// Hilfen
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Paging {
    page: Option<i64>,
    per_page: Option<i64>,
}

impl Paging {
    fn limit(&self) -> i64 {
        self.per_page.unwrap_or(100).clamp(1, 100)
    }
    fn page(&self) -> i64 {
        self.page.unwrap_or(1).max(1)
    }
    fn offset(&self) -> i64 {
        (self.page() - 1) * self.limit()
    }
}

fn paged(key: &str, items: Value, count: i64, p: &Paging, path: &str) -> Json<Value> {
    let link = |page: i64| format!("/api/v2{path}?page={page}&per_page={}", p.limit());
    let next = (p.offset() + p.limit() < count).then(|| link(p.page() + 1));
    let prev = (p.page() > 1).then(|| link(p.page() - 1));
    Json(json!({ key: items, "count": count, "next_page": next, "previous_page": prev }))
}

fn require_admin(user: &User) -> AppResult<()> {
    if user.is_admin() { Ok(()) } else { Err(AppError::Forbidden) }
}

// ---------------------------------------------------------------------------
// Tickets
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TicketFilter {
    status: Option<String>,
    assignee_id: Option<i64>,
    requester_id: Option<i64>,
    page: Option<i64>,
    per_page: Option<i64>,
}

async fn list_tickets(State(st): State<AppState>, ApiAgent(_): ApiAgent, Query(f): Query<TicketFilter>) -> JsonResult {
    let paging = Paging { page: f.page, per_page: f.per_page };
    let filter = "($1::text IS NULL OR status = $1) AND ($2::bigint IS NULL OR assignee_id = $2)
                  AND ($3::bigint IS NULL OR requester_id = $3)";
    let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM tickets WHERE {filter}"))
        .bind(&f.status)
        .bind(f.assignee_id)
        .bind(f.requester_id)
        .fetch_one(&st.db)
        .await?;
    let tickets: Vec<Ticket> =
        sqlx::query_as(&format!("SELECT * FROM tickets WHERE {filter} ORDER BY id LIMIT $4 OFFSET $5"))
            .bind(&f.status)
            .bind(f.assignee_id)
            .bind(f.requester_id)
            .bind(paging.limit())
            .bind(paging.offset())
            .fetch_all(&st.db)
            .await?;
    Ok(paged("tickets", json!(tickets), count, &paging, "/tickets"))
}

#[derive(Debug, Deserialize)]
struct TicketEnvelope<T> {
    ticket: T,
}

#[derive(Debug, Deserialize)]
struct RequesterInput {
    name: Option<String>,
    email: Option<String>,
    phone: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TicketCreate {
    subject: String,
    comment: Option<CommentChange>,
    description: Option<String>,
    requester_id: Option<i64>,
    requester: Option<RequesterInput>,
    status: Option<String>,
    priority: Option<String>,
    #[serde(rename = "type")]
    ticket_type: Option<String>,
    channel: Option<String>,
    assignee_id: Option<i64>,
    group_id: Option<i64>,
    tags: Option<Vec<String>>,
    custom_fields: Option<Value>,
    due_at: Option<chrono::DateTime<Utc>>,
}

async fn create_ticket(
    State(st): State<AppState>,
    ApiAgent(me): ApiAgent,
    JsonBody(body): JsonBody<TicketEnvelope<TicketCreate>>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let input = body.ticket;
    let requester_id = match (input.requester_id, &input.requester) {
        (Some(id), _) => id,
        (None, Some(r)) => {
            service::find_or_create_requester(
                &st.db,
                r.name.as_deref().unwrap_or(""),
                r.email.as_deref(),
                r.phone.as_deref(),
            )
            .await?
            .id
        }
        (None, None) => me.id,
    };
    let (description, public) = match (&input.comment, &input.description) {
        (Some(c), _) => (c.body.clone(), c.public),
        (None, Some(d)) => (d.clone(), true),
        (None, None) => return Err(AppError::invalid("comment.body oder description ist erforderlich")),
    };
    let ticket = service::create_ticket(
        &st.db,
        &me,
        NewTicket {
            subject: input.subject,
            description,
            public,
            requester_id,
            status: input.status,
            priority: input.priority,
            ticket_type: input.ticket_type,
            channel: input.channel.unwrap_or_else(|| "api".into()),
            assignee_id: input.assignee_id,
            group_id: input.group_id,
            tags: input.tags.unwrap_or_default(),
            custom_fields: input.custom_fields.unwrap_or_else(|| json!({})),
            due_at: input.due_at,
        },
        "api",
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "ticket": ticket }))))
}

async fn show_ticket(State(st): State<AppState>, ApiAgent(_): ApiAgent, Path(id): Path<i64>) -> JsonResult {
    Ok(Json(json!({ "ticket": service::get_ticket(&st.db, id).await? })))
}

async fn update_ticket(
    State(st): State<AppState>,
    ApiAgent(me): ApiAgent,
    Path(id): Path<i64>,
    JsonBody(body): JsonBody<TicketEnvelope<TicketChanges>>,
) -> JsonResult {
    let ticket = service::update_ticket(&st.db, &me, id, body.ticket, None, "api").await?;
    Ok(Json(json!({ "ticket": ticket })))
}

async fn delete_ticket(State(st): State<AppState>, ApiAgent(me): ApiAgent, Path(id): Path<i64>) -> AppResult<StatusCode> {
    require_admin(&me)?;
    let deleted = sqlx::query("DELETE FROM tickets WHERE id = $1").bind(id).execute(&st.db).await?;
    if deleted.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn ticket_comments(State(st): State<AppState>, ApiAgent(_): ApiAgent, Path(id): Path<i64>) -> JsonResult {
    service::get_ticket(&st.db, id).await?;
    let comments: Vec<Comment> =
        sqlx::query_as("SELECT * FROM ticket_comments WHERE ticket_id = $1 ORDER BY created_at, id")
            .bind(id)
            .fetch_all(&st.db)
            .await?;
    Ok(Json(json!({ "comments": comments, "count": comments.len() })))
}

async fn ticket_audits(State(st): State<AppState>, ApiAgent(_): ApiAgent, Path(id): Path<i64>) -> JsonResult {
    service::get_ticket(&st.db, id).await?;
    let audits: Vec<Audit> =
        sqlx::query_as("SELECT * FROM ticket_audits WHERE ticket_id = $1 ORDER BY created_at, id")
            .bind(id)
            .fetch_all(&st.db)
            .await?;
    Ok(Json(json!({ "audits": audits, "count": audits.len() })))
}

async fn ticket_sla(State(st): State<AppState>, ApiAgent(_): ApiAgent, Path(id): Path<i64>) -> JsonResult {
    let ticket = service::get_ticket(&st.db, id).await?;
    let policy = sla::load_policy(&st.db, ticket.sla_policy_id).await?;
    let targets = policy.as_ref().map(|p| sla::evaluate(p, &ticket, Utc::now())).unwrap_or_default();
    Ok(Json(json!({ "sla_policy": policy, "targets": targets })))
}

async fn apply_macro(
    State(st): State<AppState>,
    ApiAgent(me): ApiAgent,
    Path((id, macro_id)): Path<(i64, i64)>,
) -> JsonResult {
    let ticket = service::update_ticket(&st.db, &me, id, TicketChanges::default(), Some(macro_id), "api").await?;
    Ok(Json(json!({ "ticket": ticket })))
}

// ---------------------------------------------------------------------------
// Benutzer
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct UserFilter {
    role: Option<String>,
    page: Option<i64>,
    per_page: Option<i64>,
}

async fn list_users(State(st): State<AppState>, ApiAgent(_): ApiAgent, Query(f): Query<UserFilter>) -> JsonResult {
    let paging = Paging { page: f.page, per_page: f.per_page };
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE ($1::text IS NULL OR role = $1)")
        .bind(&f.role)
        .fetch_one(&st.db)
        .await?;
    let users: Vec<User> =
        sqlx::query_as("SELECT * FROM users WHERE ($1::text IS NULL OR role = $1) ORDER BY id LIMIT $2 OFFSET $3")
            .bind(&f.role)
            .bind(paging.limit())
            .bind(paging.offset())
            .fetch_all(&st.db)
            .await?;
    Ok(paged("users", json!(users), count, &paging, "/users"))
}

#[derive(Debug, Deserialize)]
struct UserEnvelope {
    user: UserInput,
}

async fn create_user(
    State(st): State<AppState>,
    ApiAgent(me): ApiAgent,
    JsonBody(body): JsonBody<UserEnvelope>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let user = service::create_user(&st.db, &me, body.user).await?;
    Ok((StatusCode::CREATED, Json(json!({ "user": user }))))
}

async fn me(ApiAgent(me): ApiAgent) -> Json<Value> {
    Json(json!({ "user": me }))
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    query: String,
}

async fn search_users(State(st): State<AppState>, ApiAgent(_): ApiAgent, Query(q): Query<SearchParams>) -> JsonResult {
    let users = service::search_users(&st.db, &q.query, 100).await?;
    Ok(Json(json!({ "users": users, "count": users.len() })))
}

async fn show_user(State(st): State<AppState>, ApiAgent(_): ApiAgent, Path(id): Path<i64>) -> JsonResult {
    let user: User = sqlx::query_as("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(json!({ "user": user })))
}

async fn update_user(
    State(st): State<AppState>,
    ApiAgent(me): ApiAgent,
    Path(id): Path<i64>,
    JsonBody(body): JsonBody<UserEnvelope>,
) -> JsonResult {
    Ok(Json(json!({ "user": service::update_user(&st.db, &me, id, body.user).await? })))
}

async fn user_tickets(State(st): State<AppState>, ApiAgent(_): ApiAgent, Path(id): Path<i64>) -> JsonResult {
    let tickets: Vec<Ticket> = sqlx::query_as("SELECT * FROM tickets WHERE requester_id = $1 ORDER BY id DESC")
        .bind(id)
        .fetch_all(&st.db)
        .await?;
    Ok(Json(json!({ "tickets": tickets, "count": tickets.len() })))
}

// ---------------------------------------------------------------------------
// Organisationen & Gruppen
// ---------------------------------------------------------------------------

async fn list_orgs(State(st): State<AppState>, ApiAgent(_): ApiAgent, Query(p): Query<Paging>) -> JsonResult {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM organizations").fetch_one(&st.db).await?;
    let orgs: Vec<Organization> = sqlx::query_as("SELECT * FROM organizations ORDER BY id LIMIT $1 OFFSET $2")
        .bind(p.limit())
        .bind(p.offset())
        .fetch_all(&st.db)
        .await?;
    Ok(paged("organizations", json!(orgs), count, &p, "/organizations"))
}

#[derive(Debug, Deserialize)]
struct OrgEnvelope {
    organization: OrgInput,
}

async fn create_org(
    State(st): State<AppState>,
    ApiAgent(me): ApiAgent,
    JsonBody(body): JsonBody<OrgEnvelope>,
) -> AppResult<(StatusCode, Json<Value>)> {
    require_admin(&me)?;
    let org = service::create_org(&st.db, body.organization).await?;
    Ok((StatusCode::CREATED, Json(json!({ "organization": org }))))
}

async fn show_org(State(st): State<AppState>, ApiAgent(_): ApiAgent, Path(id): Path<i64>) -> JsonResult {
    let org: Organization = sqlx::query_as("SELECT * FROM organizations WHERE id = $1")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(json!({ "organization": org })))
}

async fn update_org(
    State(st): State<AppState>,
    ApiAgent(me): ApiAgent,
    Path(id): Path<i64>,
    JsonBody(body): JsonBody<OrgEnvelope>,
) -> JsonResult {
    require_admin(&me)?;
    Ok(Json(json!({ "organization": service::update_org(&st.db, id, body.organization).await? })))
}

async fn list_groups(State(st): State<AppState>, ApiAgent(_): ApiAgent) -> JsonResult {
    let groups: Vec<Group> = sqlx::query_as("SELECT * FROM agent_groups ORDER BY name").fetch_all(&st.db).await?;
    Ok(Json(json!({ "groups": groups, "count": groups.len() })))
}

#[derive(Debug, Deserialize)]
struct GroupInput {
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct GroupEnvelope {
    group: GroupInput,
}

async fn create_group(
    State(st): State<AppState>,
    ApiAgent(me): ApiAgent,
    JsonBody(body): JsonBody<GroupEnvelope>,
) -> AppResult<(StatusCode, Json<Value>)> {
    require_admin(&me)?;
    let group = service::create_group(&st.db, &body.group.name, &body.group.description).await?;
    Ok((StatusCode::CREATED, Json(json!({ "group": group }))))
}

// ---------------------------------------------------------------------------
// Business-Regeln
// ---------------------------------------------------------------------------

async fn list_views(State(st): State<AppState>, ApiAgent(_): ApiAgent) -> JsonResult {
    let views: Vec<View> = sqlx::query_as("SELECT * FROM views ORDER BY position, id").fetch_all(&st.db).await?;
    Ok(Json(json!({ "views": views, "count": views.len() })))
}

async fn view_tickets(State(st): State<AppState>, ApiAgent(me): ApiAgent, Path(id): Path<i64>) -> JsonResult {
    let view: View = sqlx::query_as("SELECT * FROM views WHERE id = $1")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let all = service::candidate_tickets(&st.db).await?;
    let tickets = service::filter_view(&view, &all, &me, Utc::now());
    Ok(Json(json!({ "tickets": tickets, "count": tickets.len() })))
}

async fn list_macros(State(st): State<AppState>, ApiAgent(_): ApiAgent) -> JsonResult {
    let macros: Vec<Macro> = sqlx::query_as("SELECT * FROM macros ORDER BY position, id").fetch_all(&st.db).await?;
    Ok(Json(json!({ "macros": macros, "count": macros.len() })))
}

async fn list_triggers(State(st): State<AppState>, ApiAgent(_): ApiAgent) -> JsonResult {
    let triggers: Vec<Trigger> =
        sqlx::query_as("SELECT * FROM triggers ORDER BY position, id").fetch_all(&st.db).await?;
    Ok(Json(json!({ "triggers": triggers, "count": triggers.len() })))
}

async fn list_sla_policies(State(st): State<AppState>, ApiAgent(_): ApiAgent) -> JsonResult {
    let policies: Vec<SlaPolicy> =
        sqlx::query_as("SELECT * FROM sla_policies ORDER BY position, id").fetch_all(&st.db).await?;
    Ok(Json(json!({ "sla_policies": policies, "count": policies.len() })))
}

async fn search(State(st): State<AppState>, ApiAgent(me): ApiAgent, Query(q): Query<SearchParams>) -> JsonResult {
    let tickets = service::search_tickets(&st.db, &q.query, &me).await?;
    let results: Vec<Value> = tickets
        .into_iter()
        .map(|t| {
            let mut v = json!(t);
            v["result_type"] = json!("ticket");
            v
        })
        .collect();
    Ok(Json(json!({ "count": results.len(), "results": results })))
}
