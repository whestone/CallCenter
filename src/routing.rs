//! Automatische Zuweisung von Tickets an Agenten einer Gruppe.
//!
//! Pro Gruppe einstellbar: manuell, Round Robin oder geringste Auslastung.
//! Berücksichtigt werden nur aktive Agenten mit Status "online", deren Anzahl
//! offener Tickets unter ihrer Kapazität liegt. Tickets, für die gerade niemand
//! frei ist, bleiben in der Warteschlange und werden regelmäßig nachverteilt.
//!
//! Fähigkeiten: Trägt ein Ticket einen Tag, der zu einer Fähigkeit gehört (z. B.
//! `englisch`), kommen zuerst nur Agenten mit allen benötigten Fähigkeiten in Frage.
//! Ist nach `SKILL_TIMEOUT_MINUTES` (Standard 30) niemand Passendes frei, wird
//! ohne Fähigkeiten verteilt, damit kein Ticket liegen bleibt.

use chrono::Utc;
use serde_json::json;
use sqlx::{PgConnection, PgPool};

use crate::{
    error::AppResult,
    models::{assignment_label, AuditEvent, Group, Ticket},
};

pub fn skill_timeout_minutes() -> i64 {
    std::env::var("SKILL_TIMEOUT_MINUTES").ok().and_then(|v| v.trim().parse().ok()).filter(|m| *m >= 0).unwrap_or(30)
}

/// Fähigkeiten (id, Name), die ein Ticket anhand seiner Tags benötigt.
pub async fn required_skills(conn: &mut PgConnection, tags: &[String]) -> AppResult<Vec<(i64, String)>> {
    if tags.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_as("SELECT id, name FROM skills WHERE tags && $1 ORDER BY name")
        .bind(tags)
        .fetch_all(conn)
        .await?)
}

/// Wählt einen freien Agenten der Gruppe nach deren Verteilungsregel, der alle
/// Fähigkeiten in `skills` besitzt.
async fn pick_agent(conn: &mut PgConnection, group: &Group, skills: &[i64]) -> AppResult<Option<i64>> {
    let candidates: Vec<(i64, i64, i32)> = sqlx::query_as(
        "SELECT u.id,
                (SELECT count(*) FROM tickets t WHERE t.assignee_id = u.id AND t.status IN ('new', 'open')),
                u.capacity
         FROM group_memberships m JOIN users u ON u.id = m.user_id
         WHERE m.group_id = $1 AND u.active AND u.role IN ('agent', 'admin') AND u.availability = 'online'
           AND NOT EXISTS (
               SELECT 1 FROM unnest($2::bigint[]) AS need(skill_id)
               WHERE NOT EXISTS (SELECT 1 FROM user_skills us WHERE us.user_id = u.id AND us.skill_id = need.skill_id))
         ORDER BY u.id",
    )
    .bind(group.id)
    .bind(skills)
    .fetch_all(conn)
    .await?;
    let free: Vec<(i64, i64)> = candidates
        .into_iter()
        .filter(|(_, load, capacity)| *load < i64::from(*capacity))
        .map(|(id, load, _)| (id, load))
        .collect();
    Ok(match group.assignment.as_str() {
        "least_loaded" => free.iter().min_by_key(|(id, load)| (*load, *id)).map(|(id, _)| *id),
        "round_robin" => {
            let last = group.last_assigned_user_id.unwrap_or(0);
            free.iter().find(|(id, _)| *id > last).or(free.first()).map(|(id, _)| *id)
        }
        _ => None,
    })
}

/// Weist ein Ticket automatisch zu, falls seine Gruppe Routing nutzt.
/// Gibt das Audit-Ereignis zurück, wenn zugewiesen wurde.
pub async fn route(conn: &mut PgConnection, t: &mut Ticket) -> AppResult<Option<AuditEvent>> {
    let Some(group_id) = t.group_id else { return Ok(None) };
    if t.assignee_id.is_some() || !matches!(t.status.as_str(), "new" | "open") {
        return Ok(None);
    }
    // FOR UPDATE serialisiert gleichzeitige Zuweisungen (Round-Robin-Zeiger).
    let group: Option<Group> = sqlx::query_as("SELECT * FROM agent_groups WHERE id = $1 FOR UPDATE")
        .bind(group_id)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(group) = group.filter(|g| g.assignment != "manual") else { return Ok(None) };
    let required = required_skills(conn, &t.tags).await?;
    let ids: Vec<i64> = required.iter().map(|(id, _)| *id).collect();
    let names = required.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ");
    let (agent, note) = match pick_agent(conn, &group, &ids).await? {
        Some(a) if ids.is_empty() => (Some(a), String::new()),
        Some(a) => (Some(a), format!(" · Fähigkeiten: {names}")),
        None if !ids.is_empty() && (Utc::now() - t.created_at).num_minutes() >= skill_timeout_minutes() => (
            pick_agent(conn, &group, &[]).await?,
            format!(" · ohne passende Fähigkeit ({names}), Wartezeit überschritten"),
        ),
        None => (None, String::new()),
    };
    let Some(agent_id) = agent else { return Ok(None) };

    t.assignee_id = Some(agent_id);
    if t.status == "new" {
        t.status = "open".into();
    }
    sqlx::query("UPDATE agent_groups SET last_assigned_user_id = $2 WHERE id = $1")
        .bind(group.id)
        .bind(agent_id)
        .execute(&mut *conn)
        .await?;
    Ok(Some(AuditEvent::RuleApplied {
        kind: "routing".into(),
        id: group.id,
        title: format!("{} / {}{note}", group.name, assignment_label(&group.assignment)),
    }))
}

/// Verteilt wartende, nicht zugewiesene Tickets (älteste zuerst).
pub async fn process_queue(db: &PgPool) -> AppResult<usize> {
    let queued: Vec<i64> = sqlx::query_scalar(
        "SELECT t.id FROM tickets t JOIN agent_groups g ON g.id = t.group_id
         WHERE t.assignee_id IS NULL AND t.status IN ('new', 'open') AND g.assignment <> 'manual'
         ORDER BY t.created_at LIMIT 500",
    )
    .fetch_all(db)
    .await?;

    let mut assigned = 0;
    for id in queued {
        let mut tx = db.begin().await?;
        let ticket: Option<Ticket> = sqlx::query_as(
            "SELECT * FROM tickets WHERE id = $1 AND assignee_id IS NULL FOR UPDATE SKIP LOCKED",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(before) = ticket else { continue };
        let mut t = before.clone();
        let Some(event) = route(&mut tx, &mut t).await? else { continue };

        sqlx::query("UPDATE tickets SET assignee_id = $2, status = $3, updated_at = $4 WHERE id = $1")
            .bind(t.id)
            .bind(t.assignee_id)
            .bind(&t.status)
            .bind(Utc::now())
            .execute(&mut *tx)
            .await?;
        let mut events = vec![AuditEvent::Change {
            field: "assignee_id".into(),
            previous_value: json!(null),
            value: json!(t.assignee_id),
        }];
        if t.status != before.status {
            events.push(AuditEvent::Change {
                field: "status".into(),
                previous_value: json!(before.status),
                value: json!(t.status),
            });
        }
        events.push(event);
        sqlx::query("INSERT INTO ticket_audits (ticket_id, author_id, via, events) VALUES ($1, NULL, 'routing', $2)")
            .bind(t.id)
            .bind(sqlx::types::Json(&events))
            .execute(&mut *tx)
            .await?;
        if let Some(agent) = t.assignee_id {
            let text = format!("#{} „{}“ wurde dir automatisch zugewiesen", t.id, t.subject);
            crate::notify::notify(&mut tx, agent, Some(&t), "assigned", &text, false).await?;
        }
        tx.commit().await?;
        assigned += 1;
    }
    Ok(assigned)
}
