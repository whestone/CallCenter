//! Auto-Modus: Ist er aktiv, bekommt ein Agent automatisch die nächste Aufgabe und
//! sie öffnet sich sofort – beim Einschalten und jedes Mal, wenn er ein Ticket löst.
//!
//! Woher die Aufgaben kommen und in welcher Reihenfolge, wird pro Agent unter
//! Verwaltung → Mitarbeitende festgelegt: wartende Live-Chats und beliebige Ansichten
//! (deren Bedingungen und Sortierung gelten). Vergeben werden nur Tickets, die
//! niemandem oder dem Agenten selbst zugewiesen sind, die gerade kein anderer offen
//! hat und deren Gruppe (falls gesetzt) der Agent angehört.

use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, RwLock},
    time::{Duration, Instant},
};

use chrono::Utc;
use sqlx::PgPool;

use crate::{
    error::AppResult,
    models::{Ticket, User, View},
    presence::Presence,
    service,
};

pub const CHAT: &str = "chat";

/// Übersprungene Tickets kommen so lange nicht wieder.
const SKIP_FOR: Duration = Duration::from_secs(30 * 60);

/// Wer hat den Auto-Modus an? (für die Navigation, ohne Datenbankzugriff)
static ENABLED: RwLock<Option<HashSet<i64>>> = RwLock::new(None);
/// Übersprungen: Agent → (Ticket → seit)
static SKIPPED: Mutex<Option<HashMap<i64, HashMap<i64, Instant>>>> = Mutex::new(None);
/// Vom Auto-Modus zugewiesen (beim Überspringen wird die Zuweisung wieder gelöst).
static CLAIMED: Mutex<Option<HashSet<(i64, i64)>>> = Mutex::new(None);

pub async fn reload(db: &PgPool) -> AppResult<()> {
    let ids: Vec<i64> = sqlx::query_scalar("SELECT user_id FROM agent_auto WHERE enabled").fetch_all(db).await?;
    if let Ok(mut e) = ENABLED.write() {
        *e = Some(ids.into_iter().collect());
    }
    Ok(())
}

pub fn is_enabled(user_id: i64) -> bool {
    ENABLED.read().ok().and_then(|e| e.as_ref().map(|s| s.contains(&user_id))).unwrap_or(false)
}

pub async fn set_enabled(db: &PgPool, user_id: i64, on: bool) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO agent_auto (user_id, enabled, updated_at) VALUES ($1, $2, now())
         ON CONFLICT (user_id) DO UPDATE SET enabled = $2, updated_at = now()",
    )
    .bind(user_id)
    .bind(on)
    .execute(db)
    .await?;
    reload(db).await
}

/// Standard ohne eigene Einstellung: wartende Chats, dann alle aktiven Ansichten.
async fn default_sources(db: &PgPool) -> AppResult<Vec<String>> {
    let views: Vec<i64> = sqlx::query_scalar("SELECT id FROM views WHERE active ORDER BY position, id").fetch_all(db).await?;
    Ok(std::iter::once(CHAT.to_string()).chain(views.into_iter().map(|id| format!("view:{id}"))).collect())
}

/// Eingestellte Quellen in Reihenfolge (nur noch gültige).
pub async fn sources(db: &PgPool, user_id: i64) -> AppResult<Vec<String>> {
    let stored: Option<Option<sqlx::types::Json<Vec<String>>>> =
        sqlx::query_scalar("SELECT sources FROM agent_auto WHERE user_id = $1").bind(user_id).fetch_optional(db).await?;
    match stored.flatten() {
        Some(sqlx::types::Json(list)) => Ok(list),
        None => default_sources(db).await,
    }
}

/// Alle wählbaren Quellen: (Schlüssel, Bezeichnung).
pub async fn all_sources(db: &PgPool) -> AppResult<Vec<(String, String)>> {
    let views: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, title FROM views WHERE active ORDER BY position, id").fetch_all(db).await?;
    Ok(std::iter::once((CHAT.to_string(), "Wartende Live-Chats".to_string()))
        .chain(views.into_iter().map(|(id, title)| (format!("view:{id}"), format!("Ansicht: {title}"))))
        .collect())
}

pub async fn save_sources(db: &PgPool, user_id: i64, list: &[String]) -> AppResult<()> {
    let valid: HashSet<String> = all_sources(db).await?.into_iter().map(|(k, _)| k).collect();
    let mut clean: Vec<String> = Vec::new();
    for s in list {
        if valid.contains(s) && !clean.contains(s) {
            clean.push(s.clone());
        }
    }
    sqlx::query(
        "INSERT INTO agent_auto (user_id, sources, updated_at) VALUES ($1, $2, now())
         ON CONFLICT (user_id) DO UPDATE SET sources = $2, updated_at = now()",
    )
    .bind(user_id)
    .bind(sqlx::types::Json(clean))
    .execute(db)
    .await?;
    Ok(())
}

/// Status, nach denen es weitergeht: (Schlüssel, Bezeichnung). „Geschlossen“ zählt wie „gelöst“.
pub const ADVANCE_CHOICES: [(&str, &str); 3] = [("pending", "Wartend"), ("hold", "Angehalten"), ("solved", "Gelöst")];

pub async fn advance_on(db: &PgPool, user_id: i64) -> AppResult<Vec<String>> {
    let stored: Option<Option<sqlx::types::Json<Vec<String>>>> =
        sqlx::query_scalar("SELECT advance_on FROM agent_auto WHERE user_id = $1").bind(user_id).fetch_optional(db).await?;
    Ok(stored.flatten().map(|j| j.0).unwrap_or_else(|| vec!["solved".to_string()]))
}

/// Geht es nach diesem neuen Status zur nächsten Aufgabe?
pub async fn advances(db: &PgPool, user_id: i64, status: &str) -> AppResult<bool> {
    let list = advance_on(db, user_id).await?;
    let status = if status == "closed" { "solved" } else { status };
    Ok(list.iter().any(|s| s == status))
}

pub async fn save_advance(db: &PgPool, user_id: i64, list: &[String]) -> AppResult<()> {
    let clean: Vec<&str> = ADVANCE_CHOICES.iter().map(|(k, _)| *k).filter(|k| list.iter().any(|s| s == k)).collect();
    sqlx::query(
        "INSERT INTO agent_auto (user_id, advance_on, updated_at) VALUES ($1, $2, now())
         ON CONFLICT (user_id) DO UPDATE SET advance_on = $2, updated_at = now()",
    )
    .bind(user_id)
    .bind(sqlx::types::Json(clean))
    .execute(db)
    .await?;
    Ok(())
}

pub fn skip(user_id: i64, ticket_id: i64) {
    if let Ok(mut s) = SKIPPED.lock() {
        s.get_or_insert_with(HashMap::new).entry(user_id).or_default().insert(ticket_id, Instant::now());
    }
}

fn skipped(user_id: i64) -> HashSet<i64> {
    let Ok(mut s) = SKIPPED.lock() else { return HashSet::new() };
    let map = s.get_or_insert_with(HashMap::new).entry(user_id).or_default();
    map.retain(|_, at| at.elapsed() < SKIP_FOR);
    map.keys().copied().collect()
}

/// Hat der Auto-Modus dieses Ticket dem Agenten zugewiesen? (einmalig abfragen)
pub fn take_claim(user_id: i64, ticket_id: i64) -> bool {
    CLAIMED.lock().ok().is_some_and(|mut c| c.get_or_insert_with(HashSet::new).remove(&(user_id, ticket_id)))
}

fn remember_claim(user_id: i64, ticket_id: i64) {
    if let Ok(mut c) = CLAIMED.lock() {
        c.get_or_insert_with(HashSet::new).insert((user_id, ticket_id));
    }
}

/// Nächste Aufgabe suchen, ggf. zuweisen bzw. den Chat annehmen; liefert die Ticketnummer.
pub async fn next_task(db: &PgPool, presence: &Presence, me: &User) -> AppResult<Option<i64>> {
    let skip = skipped(me.id);
    let my_groups: HashSet<i64> = sqlx::query_scalar::<_, i64>("SELECT group_id FROM group_memberships WHERE user_id = $1")
        .bind(me.id)
        .fetch_all(db)
        .await?
        .into_iter()
        .collect();
    let mut tickets: Option<Vec<Ticket>> = None;
    let now = Utc::now();
    for source in sources(db, me.id).await? {
        if source == CHAT {
            let waiting: Vec<(i64, i64)> = sqlx::query_as(
                "SELECT id, ticket_id FROM chat_sessions WHERE status = 'waiting' ORDER BY created_at, id",
            )
            .fetch_all(db)
            .await?;
            for (chat_id, ticket_id) in waiting {
                if skip.contains(&ticket_id) {
                    continue;
                }
                if let Ok(s) = crate::chat::accept(db, chat_id, me).await {
                    return Ok(Some(s.ticket_id));
                }
            }
            continue;
        }
        let Some(view_id) = source.strip_prefix("view:").and_then(|v| v.parse::<i64>().ok()) else { continue };
        let Some(view): Option<View> = sqlx::query_as("SELECT * FROM views WHERE id = $1 AND active").bind(view_id).fetch_optional(db).await?
        else {
            continue;
        };
        if tickets.is_none() {
            tickets = Some(service::candidate_tickets(db).await?);
        }
        for t in service::filter_view(&view, tickets.as_deref().unwrap_or_default(), me, now) {
            if matches!(t.status.as_str(), "solved" | "closed") || skip.contains(&t.id) {
                continue;
            }
            if t.assignee_id.is_some_and(|a| a != me.id) {
                continue;
            }
            if t.group_id.is_some_and(|g| !my_groups.contains(&g)) {
                continue;
            }
            // Jemand anderes hat das Ticket gerade offen → nicht dazwischenfunken.
            if presence.others(t.id, me.id) {
                continue;
            }
            if t.assignee_id == Some(me.id) {
                return Ok(Some(t.id));
            }
            if service::claim_ticket(db, me, t.id).await? {
                remember_claim(me.id, t.id);
                return Ok(Some(t.id));
            }
        }
    }
    Ok(None)
}
