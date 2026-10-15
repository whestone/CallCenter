//! Dateianhänge: Hochladen (Agenten & Kundenportal) und sicheres Ausliefern.

use std::collections::HashMap;

use axum::{
    extract::{DefaultBodyLimit, Multipart},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};

use super::extract::{Path, State};
use super::*;
use crate::{
    auth::{PortalUser, WebAgent},
    service::{self, Upload, UPLOAD_LIMIT_BYTES},
};

pub fn router() -> Router<AppState> {
    let upload_limit = DefaultBodyLimit::max(UPLOAD_LIMIT_BYTES);
    Router::new()
        .route("/tickets/{id}/attachments", post(agent_upload).layer(upload_limit.clone()))
        .route("/attachments/{id}", get(agent_download))
        .route("/portal/tickets/{id}/attachments", post(portal_upload).layer(upload_limit))
        .route("/portal/attachments/{id}", get(portal_download))
}

/// Liest ein multipart-Formular: Textfelder und Dateien.
async fn read_upload(mut multipart: Multipart) -> AppResult<(HashMap<String, String>, Vec<Upload>)> {
    let mut fields = HashMap::new();
    let mut files = Vec::new();
    let bad = |_| AppError::invalid("Upload konnte nicht gelesen werden (zu groß?)");
    while let Some(field) = multipart.next_field().await.map_err(bad)? {
        let name = field.name().unwrap_or_default().to_string();
        match field.file_name().map(sanitize_filename) {
            Some(filename) if !filename.is_empty() => {
                let content_type = field.content_type().unwrap_or("application/octet-stream").to_string();
                let data = field.bytes().await.map_err(bad)?.to_vec();
                files.push(Upload { filename, content_type, data });
            }
            Some(_) => {} // leeres Dateifeld
            None => {
                fields.insert(name, field.text().await.map_err(bad)?);
            }
        }
    }
    Ok((fields, files))
}

/// Pfadanteile und Steuerzeichen aus Dateinamen entfernen.
fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.chars().filter(|c| !c.is_control() && !matches!(c, '"' | '<' | '>')).take(180).collect::<String>().trim().to_string()
}

/// Nur harmlose Typen werden im Browser angezeigt; alles andere (HTML, SVG, …)
/// wird als Download ausgeliefert, damit hochgeladene Dateien kein Skript ausführen.
fn inline_type(content_type: &str) -> Option<&'static str> {
    match content_type {
        "image/png" => Some("image/png"),
        "image/jpeg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        "application/pdf" => Some("application/pdf"),
        "text/plain" => Some("text/plain; charset=utf-8"),
        _ => None,
    }
}

fn file_response(filename: &str, content_type: &str, data: Vec<u8>) -> Response {
    let (ctype, disposition) = match inline_type(content_type) {
        Some(t) => (t, "inline"),
        None => ("application/octet-stream", "attachment"),
    };
    let ascii: String = filename.chars().map(|c| if c.is_ascii_alphanumeric() || ".-_ ".contains(c) { c } else { '_' }).collect();
    let encoded: String = filename
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b".-_".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    let mut resp = data.into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    if let Ok(v) = HeaderValue::from_str(&format!("{disposition}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; sandbox"));
    resp
}

async fn load(db: &PgPool, id: i64) -> AppResult<(i64, Option<i64>, String, String, Vec<u8>)> {
    sqlx::query_as("SELECT ticket_id, comment_id, filename, content_type, data FROM attachments WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

async fn agent_upload(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path(id): Path<i64>,
    multipart: Multipart,
) -> WebResult<Response> {
    let (fields, files) = read_upload(multipart).await?;
    let public = fields.get("public").map(String::as_str) != Some("0");
    let note = fields.get("note").cloned().unwrap_or_default();
    service::attach_files(&st.db, &me, id, public, &note, files, "web").await?;
    Ok(see_other_ok(&format!("/tickets/{id}"), "Hochgeladen"))
}

async fn agent_download(State(st): State<AppState>, WebAgent(_): WebAgent, Path(id): Path<i64>) -> WebResult<Response> {
    let (_, _, filename, content_type, data) = load(&st.db, id).await?;
    Ok(file_response(&filename, &content_type, data))
}

async fn portal_upload(
    State(st): State<AppState>,
    PortalUser(me): PortalUser,
    Path(id): Path<i64>,
    multipart: Multipart,
) -> WebResult<Response> {
    let ticket = service::get_ticket(&st.db, id).await?;
    if ticket.requester_id != me.id {
        return Err(AppError::NotFound.into());
    }
    let (fields, files) = read_upload(multipart).await?;
    let note = fields.get("note").cloned().unwrap_or_default();
    service::attach_files(&st.db, &me, id, true, &note, files, "portal").await?;
    Ok(see_other(&format!("/portal/tickets/{id}")))
}

async fn portal_download(State(st): State<AppState>, PortalUser(me): PortalUser, Path(id): Path<i64>) -> WebResult<Response> {
    let (ticket_id, comment_id, filename, content_type, data) = load(&st.db, id).await?;
    // Nur Anhänge an öffentlichen Kommentaren eigener Tickets.
    let visible: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM tickets t JOIN ticket_comments c ON c.ticket_id = t.id
                        WHERE t.id = $1 AND t.requester_id = $2 AND c.id = $3 AND c.public)",
    )
    .bind(ticket_id)
    .bind(me.id)
    .bind(comment_id)
    .fetch_one(&st.db)
    .await?;
    if !visible {
        return Err(AppError::NotFound.into());
    }
    Ok(file_response(&filename, &content_type, data))
}

/// Anhänge pro Kommentar für die Anzeige.
pub async fn attachments_by_comment(db: &PgPool, ticket_id: i64) -> AppResult<HashMap<i64, Vec<AttachmentInfo>>> {
    let list: Vec<AttachmentInfo> = sqlx::query_as(
        "SELECT id, ticket_id, comment_id, filename, content_type, size, created_at FROM attachments
         WHERE ticket_id = $1 ORDER BY id",
    )
    .bind(ticket_id)
    .fetch_all(db)
    .await?;
    let mut map: HashMap<i64, Vec<AttachmentInfo>> = HashMap::new();
    for a in list {
        if let Some(cid) = a.comment_id {
            map.entry(cid).or_default().push(a);
        }
    }
    Ok(map)
}

pub struct AttachmentView {
    pub id: i64,
    pub filename: String,
    pub size: String,
}

impl From<&AttachmentInfo> for AttachmentView {
    fn from(a: &AttachmentInfo) -> Self {
        AttachmentView { id: a.id, filename: a.filename.clone(), size: fmt_size(a.size) }
    }
}
