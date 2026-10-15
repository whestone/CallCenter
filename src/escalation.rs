//! SLA-Eskalationen: warnt vor Ablauf eines SLA-Ziels und meldet Verletzungen.
//!
//! Empfänger ist der Bearbeiter; bei unzugewiesenen Tickets die Mitglieder der
//! Gruppe, ohne Gruppe alle Administratoren. Jede Warnung bzw. Verletzung wird pro
//! SLA-Ziel (Metrik + Fälligkeit) nur einmal gemeldet.

use chrono::Utc;
use sqlx::{PgConnection, PgPool};

use crate::{error::AppResult, models::Ticket, notify, sla};

/// Vorlaufzeit der Warnung in Minuten (Verwaltung → Einstellungen, Standard 30).
pub fn warn_minutes() -> i64 {
    crate::config::int("sla.warn_minutes")
}

async fn recipients(conn: &mut PgConnection, t: &Ticket) -> AppResult<Vec<i64>> {
    if let Some(assignee) = t.assignee_id {
        return Ok(vec![assignee]);
    }
    if let Some(group) = t.group_id {
        let members: Vec<i64> = sqlx::query_scalar(
            "SELECT u.id FROM group_memberships m JOIN users u ON u.id = m.user_id WHERE m.group_id = $1 AND u.active",
        )
        .bind(group)
        .fetch_all(&mut *conn)
        .await?;
        if !members.is_empty() {
            return Ok(members);
        }
    }
    Ok(sqlx::query_scalar("SELECT id FROM users WHERE role = 'admin' AND active").fetch_all(&mut *conn).await?)
}

/// Prüft alle offenen Tickets mit SLA; gibt die Zahl der neuen Meldungen zurück.
pub async fn run(db: &PgPool) -> AppResult<usize> {
    let policies = sla::load_policies_with_schedule(db).await?;
    if policies.is_empty() {
        return Ok(0);
    }
    let tickets: Vec<Ticket> = sqlx::query_as(
        "SELECT * FROM tickets WHERE status NOT IN ('solved', 'closed') AND sla_policy_id IS NOT NULL",
    )
    .fetch_all(db)
    .await?;
    let now = Utc::now();
    let warn = warn_minutes();
    let mut conn = db.acquire().await?;
    let mut sent = 0;
    for t in &tickets {
        let Some(policy) = policies.iter().find(|p| Some(p.id) == t.sla_policy_id) else { continue };
        let Some(target) = sla::next_target(policy, t, now) else { continue };
        let key = format!("{}@{}", target.metric, target.due_at.timestamp());
        let (kind, text, column) = if target.breached {
            if t.sla_breach_key.as_deref() == Some(key.as_str()) {
                continue;
            }
            (
                "sla_breach",
                format!(
                    "SLA verletzt: #{} „{}“ – {} seit {} überfällig",
                    t.id,
                    t.subject,
                    target.label,
                    sla::fmt_minutes(-target.minutes_left)
                ),
                "sla_breach_key",
            )
        } else if target.minutes_left <= warn {
            if t.sla_warn_key.as_deref() == Some(key.as_str()) {
                continue;
            }
            (
                "sla_warning",
                format!(
                    "SLA-Warnung: #{} „{}“ – {} fällig in {}",
                    t.id,
                    t.subject,
                    target.label,
                    sla::fmt_minutes(target.minutes_left)
                ),
                "sla_warn_key",
            )
        } else {
            continue;
        };
        for user in recipients(&mut conn, t).await? {
            notify::notify(&mut conn, user, Some(t), kind, &text, true).await?;
        }
        // Bewusst ohne updated_at, damit Automationen und Listen nicht reagieren.
        sqlx::query(&format!("UPDATE tickets SET {column} = $2 WHERE id = $1"))
            .bind(t.id)
            .bind(&key)
            .execute(&mut *conn)
            .await?;
        sent += 1;
    }
    Ok(sent)
}
