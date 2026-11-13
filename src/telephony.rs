//! Telefonie, anbieterunabhängig: Anruf-Ereignisse (klingelt → angenommen → beendet)
//! von sipgate, Twilio, beliebigen Telefonanlagen oder dem Simulator werden hier
//! einheitlich verarbeitet: Anrufer erkennen, Agenten zuordnen, Ticket anlegen bzw.
//! Gesprächsnotiz ins Ticket schreiben, verpasste Anrufe als Rückruf-Ticket.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::{FromRow, PgPool};

use crate::{
    error::{AppError, AppResult},
    models::User,
    service::{self, CommentChange, NewTicket, TicketChanges},
};

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Call {
    pub id: i64,
    pub provider: String,
    pub provider_call_id: String,
    pub direction: String,
    pub from_number: String,
    pub to_number: String,
    pub customer_id: Option<i64>,
    pub agent_id: Option<i64>,
    pub ticket_id: Option<i64>,
    pub status: String,
    pub end_cause: Option<String>,
    pub recording_url: Option<String>,
    pub started_at: DateTime<Utc>,
    pub answered_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_seconds: Option<i32>,
}

impl Call {
    /// Nummer der Gegenseite (Kunde).
    pub fn remote_number(&self) -> &str {
        if self.direction == "inbound" { &self.from_number } else { &self.to_number }
    }
}

pub fn status_label(s: &str) -> &'static str {
    match s {
        "ringing" => "Klingelt",
        "answered" => "Im Gespräch",
        "completed" => "Beendet",
        "missed" => "Verpasst",
        "failed" => "Fehlgeschlagen",
        _ => "?",
    }
}

pub fn fmt_duration(seconds: Option<i32>) -> String {
    match seconds {
        Some(s) if s >= 60 => format!("{}:{:02} Min", s / 60, s % 60),
        Some(s) => format!("{s} Sek"),
        None => "–".into(),
    }
}

async fn system_user(db: &PgPool) -> AppResult<User> {
    Ok(sqlx::query_as("SELECT * FROM users WHERE email = $1")
        .bind(crate::automations::SYSTEM_EMAIL)
        .fetch_one(db)
        .await?)
}

/// Agent anhand der Anbieter-Kennung (Nebenstelle) oder E-Mail finden.
pub async fn find_agent(db: &PgPool, ident: &str) -> AppResult<Option<User>> {
    let ident = ident.trim();
    if ident.is_empty() {
        return Ok(None);
    }
    Ok(sqlx::query_as(
        "SELECT * FROM users WHERE role IN ('agent', 'admin') AND active
           AND (lower(extension) = lower($1) OR lower(email) = lower($1))
         ORDER BY id LIMIT 1",
    )
    .bind(ident)
    .fetch_optional(db)
    .await?)
}

/// Kunde anhand der Telefonnummer (nur Endkunden).
async fn find_customer(db: &PgPool, number: &str) -> AppResult<Option<i64>> {
    let phone = service::normalize_phone(number);
    if phone.chars().filter(char::is_ascii_digit).count() < 4 {
        return Ok(None);
    }
    Ok(sqlx::query_scalar("SELECT id FROM users WHERE role = 'end-user' AND phone = $1 ORDER BY id LIMIT 1")
        .bind(phone)
        .fetch_optional(db)
        .await?)
}

/// Neuer Anruf klingelt (eingehend) bzw. wird aufgebaut (ausgehend). Idempotent.
pub async fn call_started(
    db: &PgPool,
    provider: &str,
    call_id: &str,
    direction: &str,
    from: &str,
    to: &str,
    agent: Option<&User>,
) -> AppResult<Call> {
    if !matches!(direction, "inbound" | "outbound") {
        return Err(AppError::invalid("direction muss inbound oder outbound sein"));
    }
    let remote = if direction == "inbound" { from } else { to };
    let customer = find_customer(db, remote).await?;
    Ok(sqlx::query_as(
        "INSERT INTO calls (provider, provider_call_id, direction, from_number, to_number, customer_id, agent_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (provider, provider_call_id) DO UPDATE SET provider = EXCLUDED.provider
         RETURNING *",
    )
    .bind(provider)
    .bind(call_id)
    .bind(direction)
    .bind(service::normalize_phone(from))
    .bind(service::normalize_phone(to))
    .bind(customer)
    .bind(agent.map(|a| a.id))
    .fetch_one(db)
    .await?)
}

async fn get_by_provider(db: &PgPool, provider: &str, call_id: &str) -> AppResult<Call> {
    sqlx::query_as("SELECT * FROM calls WHERE provider = $1 AND provider_call_id = $2")
        .bind(provider)
        .bind(call_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

pub async fn get(db: &PgPool, id: i64) -> AppResult<Call> {
    sqlx::query_as("SELECT * FROM calls WHERE id = $1").bind(id).fetch_optional(db).await?.ok_or(AppError::NotFound)
}

/// Anruf wurde von einem Agenten angenommen.
pub async fn call_answered(db: &PgPool, provider: &str, call_id: &str, agent: Option<&User>) -> AppResult<Call> {
    let call = get_by_provider(db, provider, call_id).await?;
    Ok(sqlx::query_as(
        "UPDATE calls SET status = 'answered', answered_at = COALESCE(answered_at, now()),
            agent_id = COALESCE($2, agent_id)
         WHERE id = $1 AND status = 'ringing' RETURNING *",
    )
    .bind(call.id)
    .bind(agent.map(|a| a.id))
    .fetch_optional(db)
    .await?
    .unwrap_or(call))
}

/// Anruf beendet: Protokoll ins Ticket (vorhandenes oder neues), verpasste Anrufe als Rückruf-Ticket.
pub async fn call_ended(
    db: &PgPool,
    provider: &str,
    call_id: &str,
    cause: Option<&str>,
    duration: Option<i32>,
    recording_url: Option<&str>,
) -> AppResult<Call> {
    let call = get_by_provider(db, provider, call_id).await?;
    if matches!(call.status.as_str(), "completed" | "missed" | "failed") {
        return Ok(call); // schon verarbeitet (Anbieter senden Ereignisse teils doppelt)
    }
    let now = Utc::now();
    let was_answered = call.answered_at.is_some();
    let status = match (was_answered, cause) {
        (true, _) => "completed",
        (false, Some("failed")) => "failed",
        _ => "missed",
    };
    let duration = duration.or_else(|| call.answered_at.map(|a| (now - a).num_seconds() as i32));
    let call: Call = sqlx::query_as(
        "UPDATE calls SET status = $2, end_cause = $3, ended_at = $4, duration_seconds = $5,
            recording_url = COALESCE($6, recording_url)
         WHERE id = $1 RETURNING *",
    )
    .bind(call.id)
    .bind(status)
    .bind(cause)
    .bind(now)
    .bind(if was_answered { duration } else { None })
    .bind(recording_url)
    .fetch_one(db)
    .await?;
    let ticket_id = record_in_ticket(db, &call).await?;
    sqlx::query("UPDATE calls SET ticket_id = $2 WHERE id = $1").bind(call.id).bind(ticket_id).execute(db).await?;
    get(db, call.id).await
}

/// Schreibt das Gesprächsprotokoll als interne Notiz in das verknüpfte Ticket
/// oder legt ein neues Telefon-Ticket an. Gibt die Ticket-ID zurück.
async fn record_in_ticket(db: &PgPool, call: &Call) -> AppResult<i64> {
    let system = system_user(db).await?;
    let agent: Option<User> = match call.agent_id {
        Some(id) => sqlx::query_as("SELECT * FROM users WHERE id = $1").bind(id).fetch_optional(db).await?,
        None => None,
    };
    let actor = agent.clone().unwrap_or_else(|| system.clone());
    let dir = if call.direction == "inbound" { "Eingehender Anruf" } else { "Ausgehender Anruf" };
    let mut note = match call.status.as_str() {
        "completed" => format!(
            "📞 {dir} {} – {}, Dauer {}{}",
            call.remote_number(),
            "angenommen",
            fmt_duration(call.duration_seconds),
            agent.as_ref().map(|a| format!(", {}", a.name)).unwrap_or_default()
        ),
        "missed" if call.direction == "inbound" => format!("📞 Verpasster Anruf von {} – bitte zurückrufen.", call.remote_number()),
        _ => format!("📞 {dir} {} – nicht zustande gekommen.", call.remote_number()),
    };
    if let Some(url) = &call.recording_url {
        note.push_str(&format!("\nAufzeichnung: {url}"));
    }

    let missed = call.status == "missed" && call.direction == "inbound";
    // Ruft jemand mehrfach an, ohne durchzukommen: offenes Rückruf-Ticket ergänzen statt neu anlegen.
    let callback: Option<i64> = match (missed, call.customer_id) {
        (true, Some(customer)) => {
            sqlx::query_scalar(
                "SELECT id FROM tickets WHERE requester_id = $1 AND 'verpasster_anruf' = ANY(tags)
                   AND status IN ('new', 'open', 'pending', 'hold') ORDER BY created_at DESC LIMIT 1",
            )
            .bind(customer)
            .fetch_optional(db)
            .await?
        }
        _ => None,
    };

    // Vom Agenten während des Gesprächs angelegtes/verknüpftes Ticket ergänzen.
    if let Some(ticket_id) = call.ticket_id.or(callback) {
        let open: bool = sqlx::query_scalar("SELECT status <> 'closed' FROM tickets WHERE id = $1")
            .bind(ticket_id)
            .fetch_optional(db)
            .await?
            .unwrap_or(false);
        if open {
            let changes = TicketChanges {
                comment: Some(CommentChange { body: note, public: false, author_id: None }),
                ..Default::default()
            };
            service::update_ticket(db, &actor, ticket_id, changes, None, "phone").await?;
            return Ok(ticket_id);
        }
    }

    // Sonst neues Ticket; Anrufer ggf. als Kunde anlegen (Name = Nummer).
    let requester = match call.customer_id {
        Some(id) => id,
        None => service::find_or_create_requester(db, "", None, Some(call.remote_number())).await?.id,
    };
    let subject = if missed {
        format!("Verpasster Anruf von {}", call.remote_number())
    } else {
        format!("{dir} {}", call.remote_number())
    };
    let t = service::create_ticket(
        db,
        &actor,
        NewTicket {
            subject,
            description: note,
            public: false,
            requester_id: requester,
            channel: "phone".into(),
            // Verpasste Anrufe: neu und unzugewiesen → Routing verteilt den Rückruf.
            // Geführte Gespräche: offen beim Agenten, der den Vorgang selbst abschließt
            // (so greifen auch Pflichtfelder beim Lösen).
            assignee_id: if missed { None } else { agent.as_ref().map(|a| a.id) },
            status: Some(if missed { "new" } else { "open" }.into()),
            priority: Some(if missed { "high" } else { "normal" }.into()),
            tags: if missed { vec!["verpasster_anruf".into(), "rueckruf".into()] } else { vec!["anruf".into()] },
            custom_fields: json!({}),
            ..Default::default()
        },
        "phone",
    )
    .await?;
    Ok(t.id)
}

/// Agent verknüpft einen laufenden Anruf mit einem Ticket (z. B. nach "Ticket anlegen").
pub async fn link_ticket(db: &PgPool, call_id: i64, ticket_id: i64, agent: &User) -> AppResult<()> {
    sqlx::query("UPDATE calls SET ticket_id = $2, agent_id = COALESCE(agent_id, $3) WHERE id = $1")
        .bind(call_id)
        .bind(ticket_id)
        .bind(agent.id)
        .execute(db)
        .await?;
    Ok(())
}

/// Laufende Anrufe, die ein Agent sehen soll: klingelnde (für alle Online-Agenten)
/// und eigene Gespräche.
pub async fn active_for(db: &PgPool, agent: &User) -> AppResult<Vec<Call>> {
    Ok(sqlx::query_as(
        "SELECT * FROM calls
         WHERE (status = 'ringing' AND direction = 'inbound' AND started_at > now() - interval '10 minutes'
                AND (agent_id IS NULL OR agent_id = $1))
            OR (status = 'answered' AND agent_id = $1 AND started_at > now() - interval '4 hours')
         ORDER BY started_at",
    )
    .bind(agent.id)
    .fetch_all(db)
    .await?)
}

pub async fn for_ticket(db: &PgPool, ticket_id: i64) -> AppResult<Vec<Call>> {
    Ok(sqlx::query_as("SELECT * FROM calls WHERE ticket_id = $1 ORDER BY started_at DESC")
        .bind(ticket_id)
        .fetch_all(db)
        .await?)
}

pub async fn for_customer(db: &PgPool, customer_id: i64, limit: i64) -> AppResult<Vec<Call>> {
    Ok(sqlx::query_as("SELECT * FROM calls WHERE customer_id = $1 ORDER BY started_at DESC LIMIT $2")
        .bind(customer_id)
        .bind(limit)
        .fetch_all(db)
        .await?)
}

pub async fn recent(db: &PgPool, limit: i64) -> AppResult<Vec<Call>> {
    Ok(sqlx::query_as("SELECT * FROM calls ORDER BY started_at DESC LIMIT $1").bind(limit).fetch_all(db).await?)
}

// ---------------------------------------------------------------------------
// Ausgehende Anrufe (Click-to-Call)
// ---------------------------------------------------------------------------

pub enum DialResult {
    /// Anbieter baut den Anruf auf (Telefon des Agenten klingelt zuerst).
    Started(Call),
    /// Kein Anbieter konfiguriert: Browser/Softphone über tel:-Link.
    TelLink(String),
}

/// sipgate: https://api.sipgate.com/v2/sessions/calls (Token-ID + Personal Access Token).
fn sipgate_credentials() -> Option<(String, String)> {
    if crate::config::demo_mode() {
        return None;
    }
    Some((crate::config::opt("sipgate.token_id")?, crate::config::opt("sipgate.token")?))
}

pub fn dial_provider() -> Option<&'static str> {
    sipgate_credentials().map(|_| "sipgate")
}

pub async fn dial(db: &PgPool, agent: &User, number: &str) -> AppResult<DialResult> {
    let number = service::normalize_phone(number);
    if number.chars().filter(char::is_ascii_digit).count() < 4 {
        return Err(AppError::invalid("Ungültige Telefonnummer"));
    }
    let Some((token_id, token)) = sipgate_credentials() else {
        return Ok(DialResult::TelLink(format!("tel:{number}")));
    };
    let device = agent
        .extension
        .clone()
        .filter(|e| !e.trim().is_empty())
        .ok_or_else(|| AppError::invalid("Für Click-to-Call bitte unter Verwaltung → Team die sipgate-Gerätekennung (z. B. e0) eintragen"))?;
    let response = reqwest::Client::new()
        .post("https://api.sipgate.com/v2/sessions/calls")
        .basic_auth(token_id, Some(token))
        .json(&json!({ "deviceId": device, "callee": number }))
        .send()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(AppError::invalid(format!("sipgate lehnt den Anruf ab ({status}): {body}")));
    }
    let session: serde_json::Value = response.json().await.unwrap_or_default();
    let call_id = session["sessionId"].as_str().map(str::to_string).unwrap_or_else(|| format!("dial-{}", uuid::Uuid::new_v4().simple()));
    let call = call_started(db, "sipgate", &call_id, "outbound", agent.extension.as_deref().unwrap_or(""), &number, Some(agent)).await?;
    Ok(DialResult::Started(call))
}
