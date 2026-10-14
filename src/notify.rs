//! Benachrichtigungen für Agenten: Glocke in der Oberfläche, optional zusätzlich per E-Mail.
//! Auslöser: @Erwähnungen, Kundenantworten auf eigene Tickets, SLA-Warnungen/-Verletzungen.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgConnection, PgPool};

use crate::{email, error::AppResult, models::Ticket};

pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "mention" => "Erwähnung",
        "reply" => "Kundenantwort",
        "assigned" => "Zugewiesen",
        "sla_warning" => "SLA-Warnung",
        "sla_breach" => "SLA verletzt",
        "chat" => "Chat",
        _ => "Hinweis",
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Notification {
    pub id: i64,
    pub user_id: i64,
    pub ticket_id: Option<i64>,
    pub kind: String,
    pub body: String,
    pub read_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Benachrichtigung anlegen; mit `mail` zusätzlich an die E-Mail des Agenten.
pub async fn notify(
    conn: &mut PgConnection,
    user_id: i64,
    ticket: Option<&Ticket>,
    kind: &str,
    body: &str,
    mail: bool,
) -> AppResult<()> {
    // Gleiche ungelesene Meldung nicht doppelt anlegen (z. B. mehrere Updates kurz hintereinander).
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM notifications WHERE user_id = $1 AND ticket_id IS NOT DISTINCT FROM $2
           AND kind = $3 AND body = $4 AND read_at IS NULL)",
    )
    .bind(user_id)
    .bind(ticket.map(|t| t.id))
    .bind(kind)
    .bind(body)
    .fetch_one(&mut *conn)
    .await?;
    if exists {
        return Ok(());
    }
    sqlx::query("INSERT INTO notifications (user_id, ticket_id, kind, body) VALUES ($1, $2, $3, $4)")
        .bind(user_id)
        .bind(ticket.map(|t| t.id))
        .bind(kind)
        .bind(body)
        .execute(&mut *conn)
        .await?;
    if mail {
        let to: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = $1 AND active")
            .bind(user_id)
            .fetch_optional(&mut *conn)
            .await?
            .flatten();
        if let Some(to) = to {
            let (subject, link) = match ticket {
                Some(t) => (
                    format!("[#{}] {}: {}", t.id, kind_label(kind), t.subject),
                    format!("\n\n{}/tickets/{}", email::public_url(), t.id),
                ),
                None => (kind_label(kind).to_string(), String::new()),
            };
            email::enqueue(&mut *conn, ticket.map(|t| t.id), &to, &subject, &format!("{body}{link}")).await?;
        }
    }
    Ok(())
}

pub async fn unread_count(db: &PgPool, user_id: i64) -> AppResult<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM notifications WHERE user_id = $1 AND read_at IS NULL")
        .bind(user_id)
        .fetch_one(db)
        .await?)
}

pub async fn list(db: &PgPool, user_id: i64, limit: i64) -> AppResult<Vec<Notification>> {
    Ok(sqlx::query_as("SELECT * FROM notifications WHERE user_id = $1 ORDER BY created_at DESC, id DESC LIMIT $2")
        .bind(user_id)
        .bind(limit)
        .fetch_all(db)
        .await?)
}

/// Markiert eine Benachrichtigung als gelesen und gibt das zugehörige Ticket zurück.
pub async fn mark_read(db: &PgPool, user_id: i64, id: i64) -> AppResult<Option<i64>> {
    Ok(sqlx::query_scalar(
        "UPDATE notifications SET read_at = COALESCE(read_at, now()) WHERE id = $1 AND user_id = $2 RETURNING ticket_id",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(db)
    .await?
    .flatten())
}

pub async fn mark_all_read(db: &PgPool, user_id: i64) -> AppResult<()> {
    sqlx::query("UPDATE notifications SET read_at = now() WHERE user_id = $1 AND read_at IS NULL")
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Öffnet ein Agent ein Ticket, gelten dessen Benachrichtigungen als gelesen.
pub async fn mark_ticket_read(db: &PgPool, user_id: i64, ticket_id: i64) -> AppResult<()> {
    sqlx::query("UPDATE notifications SET read_at = now() WHERE user_id = $1 AND ticket_id = $2 AND read_at IS NULL")
        .bind(user_id)
        .bind(ticket_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Agenten, die im Text mit „@Name“ erwähnt werden (Groß-/Kleinschreibung egal).
pub fn mentioned(body: &str, agents: &[(i64, String)]) -> Vec<i64> {
    let lower = body.to_lowercase();
    agents
        .iter()
        .filter(|(_, name)| {
            let needle = format!("@{}", name.to_lowercase());
            lower.match_indices(&needle).any(|(i, _)| {
                // Danach darf kein weiterer Buchstabe folgen ("@Anna" ≠ "@Annabell").
                lower[i + needle.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric())
            })
        })
        .map(|(id, _)| *id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::mentioned;

    #[test]
    fn mentions() {
        let agents = vec![(1, "Anna Agent".to_string()), (2, "Ben".to_string()), (3, "Benedikt".to_string())];
        assert_eq!(mentioned("Hallo @anna agent, kannst du?", &agents), vec![1]);
        assert_eq!(mentioned("@Benedikt bitte", &agents), vec![3]);
        assert_eq!(mentioned("@Ben, @Benedikt", &agents), vec![2, 3]);
        assert!(mentioned("anna agent ohne at", &agents).is_empty());
    }
}
