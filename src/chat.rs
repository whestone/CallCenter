//! Live-Chat: Besucher schreiben über das Widget auf der Webseite, Agenten antworten
//! im normalen Ticket. Jeder Chat ist ein Ticket mit Kanal "chat", die Nachrichten
//! sind öffentliche Kommentare. Solange der Besucher im Chat ist, werden Antworten
//! nicht zusätzlich per E-Mail verschickt; verlässt er ihn, geht es per E-Mail weiter.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth,
    error::{AppError, AppResult},
    models::User,
    service::{self, CommentChange, NewTicket, TicketChanges},
};

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ChatSession {
    pub id: i64,
    // token_hash wird nur in SQL verglichen und nie geladen.
    pub ticket_id: i64,
    pub visitor_id: i64,
    pub agent_id: Option<i64>,
    pub status: String,
    pub page_url: String,
    pub visitor_seen_at: DateTime<Utc>,
    pub visitor_typing_at: Option<DateTime<Utc>>,
    pub agent_typing_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

pub const MAX_MESSAGE: usize = 5000;
/// So lange gilt ein Besucher ohne Lebenszeichen noch als „im Chat“.
const PRESENT_SECONDS: i64 = 90;
/// „schreibt gerade“ verfällt nach dieser Zeit.
const TYPING_SECONDS: i64 = 6;

pub fn status_label(s: &str) -> &'static str {
    match s {
        "waiting" => "wartet",
        "active" => "läuft",
        "ended" => "beendet",
        _ => "?",
    }
}

/// Ist mindestens ein Agent online (sonst nimmt das Widget nur Nachrichten an)?
pub async fn agents_online(db: &PgPool) -> AppResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE role IN ('agent', 'admin') AND active AND availability = 'online' AND email <> $1)",
    )
    .bind(crate::automations::SYSTEM_EMAIL)
    .fetch_one(db)
    .await?)
}

fn clean_message(body: &str) -> AppResult<String> {
    let body = body.trim();
    if body.is_empty() {
        return Err(AppError::invalid("Bitte eine Nachricht eingeben"));
    }
    if body.chars().count() > MAX_MESSAGE {
        return Err(AppError::invalid(format!("Nachricht zu lang (höchstens {MAX_MESSAGE} Zeichen)")));
    }
    Ok(body.to_string())
}

pub struct Started {
    pub session: ChatSession,
    pub token: String,
}

/// Neuen Chat beginnen: Kunde finden/anlegen, Ticket eröffnen, Sitzung mit Token anlegen.
pub async fn start(db: &PgPool, name: &str, email: &str, message: &str, page_url: &str) -> AppResult<Started> {
    let email = email.trim();
    if !email.contains('@') || email.len() > 200 {
        return Err(AppError::invalid("Bitte eine gültige E-Mail-Adresse angeben"));
    }
    let message = clean_message(message)?;
    let online = agents_online(db).await?;
    let visitor = service::find_or_create_requester(db, name.trim(), Some(email), None).await?;
    let first_line = message.lines().next().unwrap_or_default();
    let mut subject: String = first_line.chars().take(70).collect();
    if first_line.chars().count() > 70 {
        subject.push('…');
    }
    let ticket = service::create_ticket(
        db,
        &visitor,
        NewTicket {
            subject: format!("Chat: {subject}"),
            description: message,
            public: true,
            requester_id: visitor.id,
            channel: "chat".into(),
            tags: if online { vec!["chat".into()] } else { vec!["chat".into(), "chat_offline".into()] },
            custom_fields: json!({}),
            ..Default::default()
        },
        "chat",
    )
    .await?;
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    // Ohne Agenten online wird der Chat sofort zur normalen Anfrage (Antwort per E-Mail).
    let session: ChatSession = sqlx::query_as(
        "INSERT INTO chat_sessions (token_hash, ticket_id, visitor_id, status, page_url, ended_at)
         VALUES ($1, $2, $3, $4, $5, CASE WHEN $4 = 'ended' THEN now() END) RETURNING *",
    )
    .bind(auth::token_hash(&token))
    .bind(ticket.id)
    .bind(visitor.id)
    .bind(if online { "waiting" } else { "ended" })
    .bind(page_url.chars().take(500).collect::<String>())
    .fetch_one(db)
    .await?;
    Ok(Started { session, token })
}

/// Sitzung des Besuchers anhand seines Tokens; vermerkt das Lebenszeichen.
pub async fn by_token(db: &PgPool, token: &str) -> AppResult<ChatSession> {
    sqlx::query_as("UPDATE chat_sessions SET visitor_seen_at = now() WHERE token_hash = $1 RETURNING *")
        .bind(auth::token_hash(token.trim()))
        .fetch_optional(db)
        .await?
        // 404 statt 401: sonst könnte der Browser einen Anmeldedialog zeigen.
        .ok_or(AppError::NotFound)
}

pub async fn get(db: &PgPool, id: i64) -> AppResult<ChatSession> {
    sqlx::query_as("SELECT * FROM chat_sessions WHERE id = $1").bind(id).fetch_optional(db).await?.ok_or(AppError::NotFound)
}

pub async fn for_ticket(db: &PgPool, ticket_id: i64) -> AppResult<Option<ChatSession>> {
    Ok(sqlx::query_as("SELECT * FROM chat_sessions WHERE ticket_id = $1 ORDER BY id DESC LIMIT 1")
        .bind(ticket_id)
        .fetch_optional(db)
        .await?)
}

pub fn visitor_present(s: &ChatSession, now: DateTime<Utc>) -> bool {
    s.status != "ended" && (now - s.visitor_seen_at).num_seconds() < PRESENT_SECONDS
}

pub fn typing(at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    at.is_some_and(|t| (now - t).num_seconds() < TYPING_SECONDS)
}

#[derive(Debug, Serialize, FromRow)]
pub struct Message {
    pub id: i64,
    pub author: String,
    pub from_agent: bool,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

/// Öffentliche Nachrichten des Chats (nur diese sieht der Besucher).
pub async fn messages(db: &PgPool, ticket_id: i64, after: i64) -> AppResult<Vec<Message>> {
    Ok(sqlx::query_as(
        "SELECT c.id, u.name AS author, u.role <> 'end-user' AS from_agent, c.body, c.created_at
         FROM ticket_comments c JOIN users u ON u.id = c.author_id
         WHERE c.ticket_id = $1 AND c.public AND c.id > $2 ORDER BY c.id",
    )
    .bind(ticket_id)
    .bind(after)
    .fetch_all(db)
    .await?)
}

async fn visitor(db: &PgPool, s: &ChatSession) -> AppResult<User> {
    sqlx::query_as("SELECT * FROM users WHERE id = $1").bind(s.visitor_id).fetch_optional(db).await?.ok_or(AppError::NotFound)
}

/// Nachricht des Besuchers.
pub async fn send(db: &PgPool, s: &ChatSession, body: &str) -> AppResult<()> {
    if s.status == "ended" {
        return Err(AppError::invalid("Der Chat ist beendet. Wir antworten Ihnen per E-Mail."));
    }
    let body = clean_message(body)?;
    let user = visitor(db, s).await?;
    let changes = TicketChanges {
        comment: Some(CommentChange { body, public: true, author_id: None }),
        ..Default::default()
    };
    service::update_ticket(db, &user, s.ticket_id, changes, None, "chat").await?;
    sqlx::query("UPDATE chat_sessions SET visitor_typing_at = NULL WHERE id = $1").bind(s.id).execute(db).await?;
    Ok(())
}

pub async fn visitor_typing(db: &PgPool, s: &ChatSession) -> AppResult<()> {
    sqlx::query("UPDATE chat_sessions SET visitor_typing_at = now() WHERE id = $1").bind(s.id).execute(db).await?;
    Ok(())
}

pub async fn agent_typing(db: &PgPool, ticket_id: i64) -> AppResult<()> {
    sqlx::query("UPDATE chat_sessions SET agent_typing_at = now() WHERE ticket_id = $1 AND status <> 'ended'")
        .bind(ticket_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Agent übernimmt den Chat: Sitzung aktiv, Ticket wird ihm zugewiesen.
pub async fn accept(db: &PgPool, id: i64, agent: &User) -> AppResult<ChatSession> {
    let s = get(db, id).await?;
    if s.status == "ended" {
        return Err(AppError::invalid("Der Chat ist bereits beendet"));
    }
    if let Some(other) = s.agent_id.filter(|a| *a != agent.id && s.status == "active") {
        let name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = $1").bind(other).fetch_one(db).await?;
        return Err(AppError::invalid(format!("{name} betreut diesen Chat bereits")));
    }
    sqlx::query("UPDATE chat_sessions SET status = 'active', agent_id = $2 WHERE id = $1")
        .bind(id)
        .bind(agent.id)
        .execute(db)
        .await?;
    let changes = TicketChanges { assignee_id: Some(Some(agent.id)), ..Default::default() };
    service::update_ticket(db, agent, s.ticket_id, changes, None, "chat").await?;
    get(db, id).await
}

/// Chat beenden (Besucher oder Agent). Das Ticket bleibt offen und wird normal weiterbearbeitet.
pub async fn end(db: &PgPool, s: &ChatSession, by: &User) -> AppResult<()> {
    if s.status == "ended" {
        return Ok(());
    }
    sqlx::query("UPDATE chat_sessions SET status = 'ended', ended_at = now() WHERE id = $1").bind(s.id).execute(db).await?;
    let who = if by.is_agent() { format!("von {}", by.name) } else { "vom Besucher".to_string() };
    let changes = TicketChanges {
        comment: Some(CommentChange { body: format!("Chat {who} beendet."), public: false, author_id: None }),
        ..Default::default()
    };
    let system: User = sqlx::query_as("SELECT * FROM users WHERE email = $1")
        .bind(crate::automations::SYSTEM_EMAIL)
        .fetch_one(db)
        .await?;
    service::update_ticket(db, if by.is_agent() { by } else { &system }, s.ticket_id, changes, None, "chat").await?;
    Ok(())
}

/// Für die Ticketlogik: Läuft zu diesem Ticket gerade ein Chat mit anwesendem Besucher?
/// Dann gehen Antworten nicht zusätzlich per E-Mail raus. Eine Agenten-Antwort macht
/// einen wartenden Chat zum laufenden.
pub async fn on_agent_reply(conn: &mut PgConnection, ticket_id: i64, agent_id: i64) -> AppResult<bool> {
    let live: Option<bool> = sqlx::query_scalar(
        "UPDATE chat_sessions SET status = 'active', agent_id = COALESCE(agent_id, $2), agent_typing_at = NULL
         WHERE ticket_id = $1 AND status <> 'ended'
         RETURNING visitor_seen_at > now() - make_interval(secs => $3)",
    )
    .bind(ticket_id)
    .bind(agent_id)
    .bind(PRESENT_SECONDS as f64)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(live.unwrap_or(false))
}

/// Chats, die Aufmerksamkeit brauchen: wartend oder letzte Nachricht vom Besucher.
pub async fn attention_count(db: &PgPool, agent: &User) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT count(*) FROM chat_sessions s WHERE s.status = 'waiting'
            OR (s.status = 'active' AND s.agent_id = $1 AND (
                SELECT u.role = 'end-user' FROM ticket_comments c JOIN users u ON u.id = c.author_id
                WHERE c.ticket_id = s.ticket_id AND c.public ORDER BY c.id DESC LIMIT 1))",
    )
    .bind(agent.id)
    .fetch_one(db)
    .await?)
}

pub async fn open_sessions(db: &PgPool) -> AppResult<Vec<ChatSession>> {
    Ok(sqlx::query_as(
        "SELECT * FROM chat_sessions WHERE status <> 'ended' OR ended_at > now() - interval '1 hour'
         ORDER BY status = 'ended', created_at",
    )
    .fetch_all(db)
    .await?)
}

/// Vergessene Chats schließen: Besucher seit 30 Minuten weg.
pub async fn close_abandoned(db: &PgPool) -> AppResult<u64> {
    Ok(sqlx::query(
        "UPDATE chat_sessions SET status = 'ended', ended_at = now()
         WHERE status <> 'ended' AND visitor_seen_at < now() - interval '30 minutes'",
    )
    .execute(db)
    .await?
    .rows_affected())
}
