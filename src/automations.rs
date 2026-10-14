//! Hintergrundjobs: Routing-Warteschlange und zeitgesteuerte Automationen
//! (wie Zendesk "Automations", im Admin-Bereich konfigurierbar).

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::{
    error::AppResult,
    models::{Ticket, User},
    rules::{Action, Conditions, EvalCtx},
    service::{self, RuleActions, TicketChanges},
    AppState,
};

pub const SYSTEM_EMAIL: &str = "system@automation.invalid";

pub fn spawn(state: AppState) {
    let routing_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(20));
        loop {
            interval.tick().await;
            match crate::routing::process_queue(&routing_state.db).await {
                Ok(n) if n > 0 => tracing::info!("{n} Tickets aus der Warteschlange zugewiesen"),
                Ok(_) => {}
                Err(e) => tracing::error!("Routing fehlgeschlagen: {}", e.public_message()),
            }
        }
    });

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            match run_automations(&state.db).await {
                Ok(n) if n > 0 => tracing::info!("Automationen: {n} Tickets geändert"),
                Ok(_) => {}
                Err(e) => tracing::error!("Automationen fehlgeschlagen: {}", e.public_message()),
            }
            match crate::escalation::run(&state.db).await {
                Ok(n) if n > 0 => tracing::info!("SLA-Eskalation: {n} Meldungen"),
                Ok(_) => {}
                Err(e) => tracing::error!("SLA-Eskalation fehlgeschlagen: {}", e.public_message()),
            }
        }
    });
}

#[derive(sqlx::FromRow)]
struct Automation {
    id: i64,
    title: String,
    conditions: sqlx::types::Json<Conditions>,
    actions: sqlx::types::Json<Vec<Action>>,
}

/// Prüft alle aktiven Automationen gegen nicht geschlossene Tickets.
pub async fn run_automations(db: &sqlx::PgPool) -> AppResult<usize> {
    let system: User = sqlx::query_as("SELECT * FROM users WHERE email = $1")
        .bind(SYSTEM_EMAIL)
        .fetch_one(db)
        .await?;
    let automations: Vec<Automation> =
        sqlx::query_as("SELECT id, title, conditions, actions FROM automations WHERE active ORDER BY position, id")
            .fetch_all(db)
            .await?;
    if automations.is_empty() {
        return Ok(0);
    }
    let tickets: Vec<Ticket> = sqlx::query_as("SELECT * FROM tickets WHERE status <> 'closed'").fetch_all(db).await?;
    let runs: Vec<(i64, i64, DateTime<Utc>)> =
        sqlx::query_as("SELECT automation_id, ticket_id, ran_at FROM automation_runs").fetch_all(db).await?;

    let now = Utc::now();
    let mut changed = 0;
    for ticket in &tickets {
        for a in &automations {
            // Schon für diesen Ticketstand ausgeführt?
            let already = runs
                .iter()
                .any(|(aid, tid, ran_at)| *aid == a.id && *tid == ticket.id && ticket.updated_at <= *ran_at);
            if already || !a.conditions.0.matches(ticket, &EvalCtx::passive(None, now)) {
                continue;
            }
            let rule = RuleActions { kind: "automation", id: a.id, title: a.title.clone(), actions: a.actions.0.clone() };
            match service::update_ticket_with(db, &system, ticket.id, TicketChanges::default(), Some(rule), "automation").await {
                Ok(_) => changed += 1,
                Err(e) => tracing::warn!(ticket = ticket.id, automation = a.id, "Automation übersprungen: {}", e.public_message()),
            }
            sqlx::query(
                "INSERT INTO automation_runs (automation_id, ticket_id, ran_at) VALUES ($1, $2, now())
                 ON CONFLICT (automation_id, ticket_id) DO UPDATE SET ran_at = now()",
            )
            .bind(a.id)
            .bind(ticket.id)
            .execute(db)
            .await?;
            // Pro Durchlauf nur eine Automation je Ticket; die nächste prüft den neuen Stand.
            break;
        }
    }
    Ok(changed)
}
