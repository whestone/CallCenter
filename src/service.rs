//! Geschäftslogik, die von Web-Oberfläche und REST-API gemeinsam genutzt wird.
//!
//! Jede Ticketänderung läuft hier durch: Validierung, eingebaute Zendesk-Regeln
//! (new → open bei Zuweisung, Wiedereröffnung bei Kundenantwort …), Triggers,
//! SLA-Zuordnung und ein Audit-Eintrag mit allen Änderungen.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool};

use crate::{
    auth, email,
    error::{AppError, AppResult},
    models::*,
    routing,
    rules::{
        normalize_tags, render_placeholders, CommentInput, EvalCtx, Effects, UpdateType,
    },
    sla, webhooks,
};

/// Unterscheidet bei JSON "Feld fehlt" (None) von "Feld ist null" (Some(None)).
pub fn double_option<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

fn yes() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Tickets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct NewTicket {
    pub subject: String,
    pub description: String,
    pub public: bool,
    pub requester_id: i64,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub ticket_type: Option<String>,
    pub channel: String,
    pub assignee_id: Option<i64>,
    pub group_id: Option<i64>,
    pub tags: Vec<String>,
    pub custom_fields: Value,
    pub due_at: Option<DateTime<Utc>>,
    pub problem_id: Option<i64>,
    pub followup_source_id: Option<i64>,
    pub cc_ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TicketChanges {
    pub subject: Option<String>,
    pub status: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub priority: Option<Option<String>>,
    #[serde(default, rename = "type", deserialize_with = "double_option")]
    pub ticket_type: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub assignee_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "double_option")]
    pub group_id: Option<Option<i64>>,
    pub requester_id: Option<i64>,
    pub tags: Option<Vec<String>>,
    pub additional_tags: Option<Vec<String>>,
    pub remove_tags: Option<Vec<String>>,
    pub custom_fields: Option<Value>,
    #[serde(default, deserialize_with = "double_option")]
    pub due_at: Option<Option<DateTime<Utc>>>,
    #[serde(default, deserialize_with = "double_option")]
    pub problem_id: Option<Option<i64>>,
    pub cc_ids: Option<Vec<i64>>,
    pub comment: Option<CommentChange>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CommentChange {
    pub body: String,
    #[serde(default = "yes")]
    pub public: bool,
    /// Kommentar im Namen eines anderen Nutzers (z. B. E-Mail-Import des Kunden).
    pub author_id: Option<i64>,
}

pub async fn get_user(conn: &mut PgConnection, id: i64) -> AppResult<Option<User>> {
    Ok(sqlx::query_as("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(conn)
        .await?)
}

fn check_enum(value: Option<&str>, allowed: &[&str], what: &str) -> AppResult<()> {
    match value {
        Some(v) if !allowed.contains(&v) => Err(AppError::invalid(format!("Ungültiger Wert für {what}: {v}"))),
        _ => Ok(()),
    }
}

fn validate_enums(t: &Ticket) -> AppResult<()> {
    check_enum(Some(&t.status), &STATUSES, "Status")?;
    check_enum(t.priority.as_deref(), &PRIORITIES, "Priorität")?;
    check_enum(t.ticket_type.as_deref(), &TYPES, "Typ")?;
    check_enum(Some(&t.channel), &CHANNELS, "Kanal")
}

fn snapshot(t: &Ticket) -> Vec<(&'static str, Value)> {
    vec![
        ("subject", json!(t.subject)),
        ("status", json!(t.status)),
        ("substatus", json!(t.substatus)),
        ("priority", json!(t.priority)),
        ("type", json!(t.ticket_type)),
        ("requester_id", json!(t.requester_id)),
        ("assignee_id", json!(t.assignee_id)),
        ("group_id", json!(t.group_id)),
        ("organization_id", json!(t.organization_id)),
        ("tags", json!(t.tags)),
        ("sla_policy_id", json!(t.sla_policy_id)),
        ("due_at", json!(t.due_at)),
        ("problem_id", json!(t.problem_id)),
        ("cc_ids", json!(t.cc_ids)),
    ]
}

fn custom_value(t: &Ticket, key: &str) -> Value {
    t.custom_fields.get(key).cloned().unwrap_or(Value::Null)
}

fn create_events(t: &Ticket) -> Vec<AuditEvent> {
    let mut events: Vec<AuditEvent> = snapshot(t)
        .into_iter()
        .filter(|(_, v)| !(v.is_null() || *v == json!([]) || *v == json!({})))
        .map(|(field, value)| AuditEvent::Create { field: field.into(), value })
        .collect();
    if let Some(obj) = t.custom_fields.as_object() {
        events.extend(obj.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| AuditEvent::Create {
            field: format!("custom_fields.{k}"),
            value: v.clone(),
        }));
    }
    events
}

fn diff_events(before: &Ticket, after: &Ticket) -> Vec<AuditEvent> {
    let mut events: Vec<AuditEvent> = snapshot(before)
        .into_iter()
        .zip(snapshot(after))
        .filter(|((_, a), (_, b))| a != b)
        .map(|((field, previous_value), (_, value))| AuditEvent::Change {
            field: field.into(),
            previous_value,
            value,
        })
        .collect();
    // Eigene Felder einzeln protokollieren.
    let mut keys: Vec<&String> = before
        .custom_fields
        .as_object()
        .into_iter()
        .chain(after.custom_fields.as_object())
        .flat_map(|o| o.keys())
        .collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let (previous_value, value) = (custom_value(before, key), custom_value(after, key));
        if previous_value != value {
            events.push(AuditEvent::Change { field: format!("custom_fields.{key}"), previous_value, value });
        }
    }
    events
}

#[allow(clippy::too_many_arguments)]
async fn run_triggers(
    conn: &mut PgConnection,
    t: &mut Ticket,
    before: Option<&Ticket>,
    update_type: UpdateType,
    comment: Option<&CommentInput>,
    actor: &User,
    fx: &mut Effects,
    events: &mut Vec<AuditEvent>,
) -> AppResult<()> {
    let triggers: Vec<Trigger> =
        sqlx::query_as("SELECT * FROM triggers WHERE active ORDER BY position, id")
            .fetch_all(&mut *conn)
            .await?;
    let now = Utc::now();
    let mut fired = HashSet::new();
    // Wie bei Zendesk: nach jeder Änderung werden die Triggers erneut geprüft,
    // jeder Trigger feuert aber höchstens einmal pro Update.
    for _ in 0..10 {
        let mut any_fired = false;
        for trigger in &triggers {
            if fired.contains(&trigger.id) {
                continue;
            }
            let ctx = EvalCtx { before, update_type, comment, actor: Some(actor), now };
            if !trigger.conditions.0.matches(t, &ctx) {
                continue;
            }
            fired.insert(trigger.id);
            any_fired = true;
            for action in &trigger.actions.0 {
                if let Err(e) = action.apply(t, Some(actor), fx) {
                    tracing::warn!(trigger = trigger.id, "Trigger-Aktion fehlgeschlagen: {e}");
                }
            }
            events.push(AuditEvent::RuleApplied {
                kind: "trigger".into(),
                id: trigger.id,
                title: trigger.title.clone(),
            });
        }
        if !any_fired {
            break;
        }
    }
    Ok(())
}

/// Referenzen prüfen, abgeleitete Felder setzen, SLA zuordnen.
async fn finalize(
    conn: &mut PgConnection,
    t: &mut Ticket,
    before: Option<&Ticket>,
    now: DateTime<Utc>,
) -> AppResult<()> {
    validate_enums(t)?;

    if let Some(assignee_id) = t.assignee_id {
        let assignee = get_user(conn, assignee_id)
            .await?
            .ok_or_else(|| AppError::invalid("Bearbeiter existiert nicht"))?;
        // Bestehende Zuweisungen an inzwischen deaktivierte Agenten bleiben erhalten,
        // nur neue Zuweisungen müssen auf aktive Agenten gehen.
        let newly_assigned = before.is_none_or(|b| b.assignee_id != t.assignee_id);
        if !assignee.is_agent() || (newly_assigned && !assignee.active) {
            return Err(AppError::invalid("Bearbeiter muss ein aktiver Agent sein"));
        }
        if t.group_id.is_none() {
            t.group_id = sqlx::query_scalar(
                "SELECT group_id FROM group_memberships WHERE user_id = $1 ORDER BY group_id LIMIT 1",
            )
            .bind(assignee_id)
            .fetch_optional(&mut *conn)
            .await?;
        }
    }
    if let Some(group_id) = t.group_id {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent_groups WHERE id = $1)")
            .bind(group_id)
            .fetch_one(&mut *conn)
            .await?;
        if !exists {
            return Err(AppError::invalid("Gruppe existiert nicht"));
        }
    }

    // Nur Vorfälle hängen an einem Problem; das Problem muss existieren.
    if t.ticket_type.as_deref() != Some("incident") {
        t.problem_id = None;
    }
    if let Some(problem_id) = t.problem_id.filter(|p| before.is_none_or(|b| b.problem_id != Some(*p))) {
        if problem_id == t.id {
            return Err(AppError::invalid("Ein Ticket kann nicht sein eigenes Problem sein"));
        }
        let problem: Option<(Option<String>, String)> =
            sqlx::query_as("SELECT ticket_type, status FROM tickets WHERE id = $1")
                .bind(problem_id)
                .fetch_optional(&mut *conn)
                .await?;
        match problem {
            None => return Err(AppError::invalid(format!("Problem-Ticket #{problem_id} existiert nicht"))),
            Some((ty, _)) if ty.as_deref() != Some("problem") => {
                return Err(AppError::invalid(format!("Ticket #{problem_id} hat nicht den Typ „Problem“")))
            }
            Some((_, status)) if status == "closed" => {
                return Err(AppError::invalid(format!("Problem-Ticket #{problem_id} ist bereits geschlossen")))
            }
            _ => {}
        }
    }

    // CC: eindeutig, ohne den Anfragenden, nur existierende Personen mit E-Mail.
    t.cc_ids.sort_unstable();
    t.cc_ids.dedup();
    t.cc_ids.retain(|id| *id != t.requester_id);
    if !t.cc_ids.is_empty() {
        let valid: Vec<i64> = sqlx::query_scalar("SELECT id FROM users WHERE id = ANY($1) AND email IS NOT NULL")
            .bind(&t.cc_ids)
            .fetch_all(&mut *conn)
            .await?;
        if valid.len() != t.cc_ids.len() {
            return Err(AppError::invalid("CC: Person existiert nicht oder hat keine E-Mail-Adresse"));
        }
    }

    if before.is_none_or(|b| b.requester_id != t.requester_id) {
        t.organization_id = sqlx::query_scalar("SELECT organization_id FROM users WHERE id = $1")
            .bind(t.requester_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| AppError::invalid("Anfragender existiert nicht"))?;
    }

    // Pflichtfelder müssen beim Lösen ausgefüllt sein (nur beim Übergang, damit z. B.
    // das automatische Schließen alter gelöster Tickets nicht blockiert wird).
    let becomes_solved = matches!(t.status.as_str(), "solved" | "closed")
        && !before.is_some_and(|b| matches!(b.status.as_str(), "solved" | "closed"));
    if becomes_solved {
        let required: Vec<(String, String)> =
            sqlx::query_as("SELECT key, label FROM ticket_fields WHERE active AND required_on_solve ORDER BY position, id")
                .fetch_all(&mut *conn)
                .await?;
        let missing: Vec<String> = required
            .into_iter()
            .filter(|(key, _)| match t.custom_fields.get(key) {
                None | Some(Value::Null) => true,
                Some(Value::String(s)) => s.trim().is_empty(),
                _ => false,
            })
            .map(|(_, label)| format!("„{label}“"))
            .collect();
        if !missing.is_empty() {
            return Err(AppError::invalid(format!(
                "Vor dem Lösen ausfüllen: {}",
                missing.join(", ")
            )));
        }
    }

    match t.status.as_str() {
        "solved" | "closed" => {
            if t.solved_at.is_none() {
                t.solved_at = Some(now);
            }
        }
        _ => t.solved_at = None,
    }

    let policies = sla::load_policies(&mut *conn).await?;
    let ctx = EvalCtx::passive(None, now);
    t.sla_policy_id = policies.iter().find(|p| p.filter.0.matches(t, &ctx)).map(|p| p.id);
    t.updated_at = now;
    Ok(())
}

async fn insert_comment(
    conn: &mut PgConnection,
    ticket_id: i64,
    author_id: i64,
    body: &str,
    public: bool,
    via: &str,
) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "INSERT INTO ticket_comments (ticket_id, author_id, body, public, via) VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(ticket_id)
    .bind(author_id)
    .bind(body)
    .bind(public)
    .bind(via)
    .fetch_one(conn)
    .await?)
}

async fn insert_audit(
    conn: &mut PgConnection,
    ticket_id: i64,
    author_id: Option<i64>,
    via: &str,
    events: &[AuditEvent],
) -> AppResult<()> {
    sqlx::query("INSERT INTO ticket_audits (ticket_id, author_id, via, events) VALUES ($1, $2, $3, $4)")
        .bind(ticket_id)
        .bind(author_id)
        .bind(via)
        .bind(sqlx::types::Json(events))
        .execute(conn)
        .await?;
    Ok(())
}

/// Speichert ausstehende Kommentare und Benachrichtigungen eines Updates.
#[allow(clippy::too_many_arguments)]
async fn write_effects(
    conn: &mut PgConnection,
    t: &Ticket,
    actor: &User,
    comments: Vec<(i64, CommentInput, String)>,
    notifications: Vec<String>,
    events: &mut Vec<AuditEvent>,
    update_via: &str,
    created: bool,
) -> AppResult<()> {
    // Demodaten verschicken keine E-Mails.
    let send_mail = update_via != "seed";
    let agents: Vec<(i64, String)> = if comments.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as("SELECT id, name FROM users WHERE role IN ('agent', 'admin') AND active AND email <> $1")
            .bind(crate::automations::SYSTEM_EMAIL)
            .fetch_all(&mut *conn)
            .await?
    };
    for (author_id, c, via) in comments {
        let body = render_placeholders(&c.body, t, Some(actor));
        let comment_id = insert_comment(conn, t.id, author_id, &body, c.public, &via).await?;
        events.push(AuditEvent::Comment { comment_id, public: c.public });
        let author: Option<(String, String)> = sqlx::query_as("SELECT name, role FROM users WHERE id = $1")
            .bind(author_id)
            .fetch_optional(&mut *conn)
            .await?;
        let (author_name, author_is_agent) = author.map(|(n, r)| (n, r != "end-user")).unwrap_or_default();
        if send_mail && c.public && author_id != t.requester_id && author_is_agent {
            // Im laufenden Live-Chat liest der Kunde direkt mit – keine zusätzliche E-Mail.
            let live_chat = t.channel == "chat" && crate::chat::on_agent_reply(conn, t.id, author_id).await?;
            if !live_chat {
                email::queue_ticket_mail(conn, t, email::TicketMail::Reply { body: &body }).await?;
            }
        }
        if !send_mail {
            continue;
        }
        if author_is_agent {
            for user_id in crate::notify::mentioned(&body, &agents).into_iter().filter(|id| *id != author_id) {
                let text = format!("{author_name} hat dich in #{} „{}“ erwähnt", t.id, t.subject);
                crate::notify::notify(conn, user_id, Some(t), "mention", &text, true).await?;
            }
        } else if let Some(assignee) = t.assignee_id.filter(|_| c.public && !created && t.channel != "chat") {
            let text = format!("{author_name} hat auf #{} „{}“ geantwortet", t.id, t.subject);
            crate::notify::notify(conn, assignee, Some(t), "reply", &text, false).await?;
        }
    }
    for n in notifications {
        let body = render_placeholders(&n, t, Some(actor));
        if send_mail {
            email::queue_ticket_mail(conn, t, email::TicketMail::Notification { body: &body }).await?;
        }
        events.push(AuditEvent::Notification { body });
    }
    Ok(())
}

pub async fn create_ticket(db: &PgPool, actor: &User, input: NewTicket, via: &str) -> AppResult<Ticket> {
    let subject = input.subject.trim().to_string();
    if subject.is_empty() {
        return Err(AppError::invalid("Betreff darf nicht leer sein"));
    }
    if input.description.trim().is_empty() {
        return Err(AppError::invalid("Beschreibung darf nicht leer sein"));
    }
    let status = input.status.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| "new".into());
    if status == "closed" {
        return Err(AppError::invalid("Tickets können nicht direkt geschlossen angelegt werden"));
    }

    let mut tx = db.begin().await?;
    let requester = get_user(&mut tx, input.requester_id)
        .await?
        .ok_or_else(|| AppError::invalid("Anfragender existiert nicht"))?;

    let now = Utc::now();
    let mut t = Ticket {
        id: 0,
        subject,
        description: input.description.trim().to_string(),
        status,
        priority: input.priority.filter(|s| !s.is_empty()),
        ticket_type: input.ticket_type.filter(|s| !s.is_empty()),
        channel: if input.channel.is_empty() { "web".into() } else { input.channel },
        requester_id: requester.id,
        submitter_id: actor.id,
        assignee_id: input.assignee_id,
        group_id: input.group_id,
        organization_id: requester.organization_id,
        tags: normalize_tags(&input.tags),
        custom_fields: if input.custom_fields.is_object() { input.custom_fields } else { json!({}) },
        sla_policy_id: None,
        due_at: input.due_at,
        first_reply_at: None,
        solved_at: None,
        created_at: now,
        updated_at: now,
        satisfaction_score: None,
        satisfaction_comment: None,
        satisfaction_at: None,
        problem_id: input.problem_id,
        followup_source_id: input.followup_source_id,
        cc_ids: input.cc_ids,
        sla_warn_key: None,
        sla_breach_key: None,
        sla_paused_seconds: 0,
        sla_paused_since: None,
        substatus: None,
    };
    validate_enums(&t)?;
    if t.status == "new" && t.assignee_id.is_some() {
        t.status = "open".into();
    }

    // Bei Telefon-Tickets schreibt der Agent die Gesprächsnotiz selbst.
    let first_author = if t.channel == "phone" && actor.is_agent() { actor.id } else { requester.id };
    let first_comment = CommentInput { body: t.description.clone(), public: input.public };

    let mut fx = Effects::default();
    let mut rule_events = Vec::new();
    run_triggers(&mut tx, &mut t, None, UpdateType::Create, Some(&first_comment), actor, &mut fx, &mut rule_events)
        .await?;
    if let Some(event) = routing::route(&mut tx, &mut t).await? {
        rule_events.push(event);
    }
    finalize(&mut tx, &mut t, None, now).await?;

    let t: Ticket = sqlx::query_as(
        "INSERT INTO tickets (subject, description, status, priority, ticket_type, channel, requester_id,
            submitter_id, assignee_id, group_id, organization_id, tags, custom_fields, sla_policy_id,
            due_at, first_reply_at, solved_at, created_at, updated_at, problem_id, followup_source_id, cc_ids)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22)
         RETURNING *",
    )
    .bind(&t.subject)
    .bind(&t.description)
    .bind(&t.status)
    .bind(&t.priority)
    .bind(&t.ticket_type)
    .bind(&t.channel)
    .bind(t.requester_id)
    .bind(t.submitter_id)
    .bind(t.assignee_id)
    .bind(t.group_id)
    .bind(t.organization_id)
    .bind(&t.tags)
    .bind(&t.custom_fields)
    .bind(t.sla_policy_id)
    .bind(t.due_at)
    .bind(t.first_reply_at)
    .bind(t.solved_at)
    .bind(t.created_at)
    .bind(t.updated_at)
    .bind(t.problem_id)
    .bind(t.followup_source_id)
    .bind(&t.cc_ids)
    .fetch_one(&mut *tx)
    .await?;

    let mut events = create_events(&t);
    let mut comments = vec![(first_author, first_comment, via.to_string())];
    comments.extend(fx.comments.into_iter().map(|c| (actor.id, c, "trigger".to_string())));
    events.extend(rule_events);
    // Neu angelegtes Ticket, das (z. B. per Routing) jemand anderem zugewiesen wurde.
    if let Some(assignee) = t.assignee_id.filter(|a| *a != actor.id && via != "seed") {
        let text = format!("#{} „{}“ wurde dir zugewiesen", t.id, t.subject);
        crate::notify::notify(&mut tx, assignee, Some(&t), "assigned", &text, false).await?;
    }
    write_effects(&mut tx, &t, actor, comments, fx.notifications, &mut events, via, true).await?;
    insert_audit(&mut tx, t.id, Some(actor.id), via, &events).await?;
    webhooks::enqueue(&mut tx, "ticket.created", &t).await?;
    tx.commit().await?;
    Ok(t)
}

/// Regelaktionen, die bei einem Update angewendet werden (Makro oder Automation).
pub struct RuleActions {
    pub kind: &'static str,
    pub id: i64,
    pub title: String,
    pub actions: Vec<crate::rules::Action>,
}

pub async fn update_ticket(
    db: &PgPool,
    actor: &User,
    id: i64,
    changes: TicketChanges,
    macro_id: Option<i64>,
    via: &str,
) -> AppResult<Ticket> {
    let rule = match macro_id {
        Some(macro_id) => {
            // Persönliche Makros darf nur ihr Besitzer anwenden.
            let m: Macro = sqlx::query_as("SELECT * FROM macros WHERE id = $1 AND active AND (owner_id IS NULL OR owner_id = $2)")
                .bind(macro_id)
                .bind(actor.id)
                .fetch_optional(db)
                .await?
                .ok_or(AppError::NotFound)?;
            Some(RuleActions { kind: "macro", id: m.id, title: m.title, actions: m.actions.0 })
        }
        None => None,
    };
    update_ticket_with(db, actor, id, changes, rule, via).await
}

pub async fn update_ticket_with(
    db: &PgPool,
    actor: &User,
    id: i64,
    changes: TicketChanges,
    rule: Option<RuleActions>,
    via: &str,
) -> AppResult<Ticket> {
    let mut tx = db.begin().await?;
    let before: Ticket = sqlx::query_as("SELECT * FROM tickets WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;
    if before.status == "closed" {
        return Err(AppError::invalid("Geschlossene Tickets können nicht mehr bearbeitet werden"));
    }

    let now = Utc::now();
    let mut t = before.clone();
    let mut fx = Effects::default();
    let mut rule_events = Vec::new();

    let comment = changes.comment.as_ref().filter(|c| !c.body.trim().is_empty());
    let author = match comment.and_then(|c| c.author_id) {
        Some(author_id) if author_id != actor.id => get_user(&mut tx, author_id)
            .await?
            .ok_or_else(|| AppError::invalid("Kommentar-Autor existiert nicht"))?,
        _ => actor.clone(),
    };

    // 1. Direkte Feldänderungen
    if let Some(subject) = &changes.subject {
        if subject.trim().is_empty() {
            return Err(AppError::invalid("Betreff darf nicht leer sein"));
        }
        t.subject = subject.trim().to_string();
    }
    if let Some(status) = &changes.status {
        if status == "new" && before.status != "new" {
            return Err(AppError::invalid("Ein Ticket kann nicht auf 'Neu' zurückgesetzt werden"));
        }
        t.status = status.clone();
    }
    if let Some(p) = &changes.priority {
        t.priority = p.clone().filter(|s| !s.is_empty());
    }
    if let Some(ty) = &changes.ticket_type {
        t.ticket_type = ty.clone().filter(|s| !s.is_empty());
    }
    if let Some(a) = changes.assignee_id {
        t.assignee_id = a;
    }
    if let Some(g) = changes.group_id {
        t.group_id = g;
    }
    if let Some(r) = changes.requester_id {
        t.requester_id = r;
    }
    if let Some(tags) = &changes.tags {
        t.tags = normalize_tags(tags);
    }
    if let Some(add) = &changes.additional_tags {
        for tag in normalize_tags(add) {
            if !t.tags.contains(&tag) {
                t.tags.push(tag);
            }
        }
    }
    if let Some(remove) = &changes.remove_tags {
        let remove = normalize_tags(remove);
        t.tags.retain(|x| !remove.contains(x));
    }
    if let Some(cf) = &changes.custom_fields {
        match (t.custom_fields.as_object_mut(), cf.as_object()) {
            (Some(dst), Some(src)) => {
                for (k, v) in src {
                    dst.insert(k.clone(), v.clone());
                }
            }
            (None, Some(_)) => t.custom_fields = cf.clone(),
            _ => return Err(AppError::invalid("custom_fields muss ein Objekt sein")),
        }
    }
    if let Some(due) = changes.due_at {
        t.due_at = due;
    }
    if let Some(problem) = changes.problem_id {
        t.problem_id = problem;
    }
    if let Some(cc) = &changes.cc_ids {
        t.cc_ids = cc.clone();
    }

    // 2. Makro bzw. Automation anwenden
    if let Some(rule) = &rule {
        for action in &rule.actions {
            action.apply(&mut t, Some(actor), &mut fx).map_err(AppError::Unprocessable)?;
        }
        rule_events.push(AuditEvent::RuleApplied { kind: rule.kind.into(), id: rule.id, title: rule.title.clone() });
    }

    let mut comments: Vec<(i64, CommentInput, String)> = Vec::new();
    if let Some(c) = comment {
        comments.push((author.id, CommentInput { body: c.body.trim().to_string(), public: c.public }, via.into()));
    }
    let rule_via = rule.as_ref().map_or("macro", |r| r.kind);
    comments.extend(fx.comments.drain(..).map(|c| (actor.id, c, rule_via.to_string())));
    let ctx_comment = comments.first().map(|(_, c, _)| c.clone());
    let public_comment = ctx_comment.as_ref().is_some_and(|c| c.public);

    // 3. Eingebaute Regeln (Zendesk-Standardverhalten)
    if t.status == "new" && (t.assignee_id.is_some() || (author.is_agent() && public_comment)) {
        t.status = "open".into();
    }
    if !author.is_agent()
        && public_comment
        && changes.status.is_none()
        && matches!(t.status.as_str(), "pending" | "solved" | "hold")
    {
        t.status = "open".into();
    }

    // 4. Triggers
    run_triggers(&mut tx, &mut t, Some(&before), UpdateType::Change, ctx_comment.as_ref(), &author, &mut fx, &mut rule_events)
        .await?;
    comments.extend(fx.comments.drain(..).map(|c| (actor.id, c, "trigger".to_string())));
    if let Some(event) = routing::route(&mut tx, &mut t).await? {
        rule_events.push(event);
    }

    if t.first_reply_at.is_none() {
        let agent_replied = comments.iter().any(|(author_id, c, _)| {
            c.public && ((*author_id == author.id && author.is_agent()) || (*author_id == actor.id && actor.is_agent()))
        });
        if agent_replied {
            t.first_reply_at = Some(now);
        }
    }

    finalize(&mut tx, &mut t, Some(&before), now).await?;

    let mut events = diff_events(&before, &t);
    events.extend(rule_events);
    if events.is_empty() && comments.is_empty() && fx.notifications.is_empty() {
        return Ok(before);
    }

    sqlx::query(
        "UPDATE tickets SET subject = $2, status = $3, priority = $4, ticket_type = $5, requester_id = $6,
            assignee_id = $7, group_id = $8, organization_id = $9, tags = $10, custom_fields = $11,
            sla_policy_id = $12, due_at = $13, first_reply_at = $14, solved_at = $15, updated_at = $16,
            problem_id = $17, cc_ids = $18, substatus = $19
         WHERE id = $1",
    )
    .bind(t.id)
    .bind(&t.subject)
    .bind(&t.status)
    .bind(&t.priority)
    .bind(&t.ticket_type)
    .bind(t.requester_id)
    .bind(t.assignee_id)
    .bind(t.group_id)
    .bind(t.organization_id)
    .bind(&t.tags)
    .bind(&t.custom_fields)
    .bind(t.sla_policy_id)
    .bind(t.due_at)
    .bind(t.first_reply_at)
    .bind(t.solved_at)
    .bind(t.updated_at)
    .bind(t.problem_id)
    .bind(&t.cc_ids)
    .bind(&t.substatus)
    .execute(&mut *tx)
    .await?;

    // Lösungstext eines Problems wird an alle Vorfälle weitergegeben.
    let solve_note = comments.iter().find(|(_, c, _)| c.public).map(|(_, c, _)| c.body.clone());
    if let Some(assignee) = t.assignee_id.filter(|a| t.assignee_id != before.assignee_id && *a != actor.id) {
        let text = format!("{} hat dir #{} „{}“ zugewiesen", actor.name, t.id, t.subject);
        crate::notify::notify(&mut tx, assignee, Some(&t), "assigned", &text, false).await?;
    }
    write_effects(&mut tx, &t, actor, comments, std::mem::take(&mut fx.notifications), &mut events, via, false).await?;
    insert_audit(&mut tx, t.id, Some(actor.id), via, &events).await?;
    webhooks::enqueue(&mut tx, "ticket.updated", &t).await?;
    if t.status == "solved" && before.status != "solved" {
        webhooks::enqueue(&mut tx, "ticket.solved", &t).await?;
    }
    tx.commit().await?;
    if t.ticket_type.as_deref() == Some("problem") && t.status == "solved" && before.status != "solved" {
        solve_incidents(db, actor, &t, solve_note).await?;
    }
    Ok(t)
}

/// Art einer Verknüpfung aus Sicht des aktuellen Tickets.
pub const LINK_KINDS: [(&str, &str); 3] = [
    ("related", "verwandt"),
    ("problem", "ist Vorfall von … (Ursache)"),
    ("incident", "hat Vorfall …"),
];

/// Verknüpft zwei Tickets. `kind`: "related" (gleichrangig), "problem" (das andere Ticket
/// ist die Ursache; Typen werden auf Vorfall/Problem gesetzt) oder "incident" (umgekehrt).
pub async fn link_tickets(db: &PgPool, actor: &User, id: i64, other: i64, kind: &str) -> AppResult<()> {
    if id == other {
        return Err(AppError::invalid("Ein Ticket kann nicht mit sich selbst verknüpft werden"));
    }
    let me = get_ticket(db, id).await?;
    let target = get_ticket(db, other)
        .await
        .map_err(|_| AppError::invalid(format!("Ticket #{other} existiert nicht")))?;
    match kind {
        "related" => {
            let mut tx = db.begin().await?;
            let inserted = sqlx::query(
                "INSERT INTO ticket_links (a_id, b_id, created_by) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            )
            .bind(id.min(other))
            .bind(id.max(other))
            .bind(actor.id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if inserted == 0 {
                return Err(AppError::invalid(format!("Ticket #{other} ist bereits verknüpft")));
            }
            for (a, b) in [(id, other), (other, id)] {
                let ev = AuditEvent::Link { ticket_id: b, relation: "related".into(), added: true };
                insert_audit(&mut tx, a, Some(actor.id), "web", &[ev]).await?;
            }
            tx.commit().await?;
            Ok(())
        }
        "problem" | "incident" => {
            let (incident, problem) = if kind == "problem" { (&me, &target) } else { (&target, &me) };
            if incident.ticket_type.as_deref() == Some("problem") {
                let n: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets WHERE problem_id = $1")
                    .bind(incident.id)
                    .fetch_one(db)
                    .await?;
                if n > 0 {
                    return Err(AppError::invalid(format!(
                        "#{} ist ein Problem mit eigenen Vorfällen und kann kein Vorfall werden",
                        incident.id
                    )));
                }
            }
            if problem.ticket_type.as_deref() == Some("incident") && problem.problem_id.is_some() {
                return Err(AppError::invalid(format!("#{} ist selbst ein Vorfall eines anderen Problems", problem.id)));
            }
            if problem.ticket_type.as_deref() != Some("problem") {
                let changes = TicketChanges { ticket_type: Some(Some("problem".into())), ..Default::default() };
                update_ticket(db, actor, problem.id, changes, None, "web").await?;
            }
            let changes = TicketChanges {
                ticket_type: Some(Some("incident".into())),
                problem_id: Some(Some(problem.id)),
                ..Default::default()
            };
            update_ticket(db, actor, incident.id, changes, None, "web").await?;
            Ok(())
        }
        _ => Err(AppError::invalid("Unbekannte Art der Verknüpfung")),
    }
}

/// Hebt eine Verknüpfung zwischen zwei Tickets auf (verwandt oder Problem ↔ Vorfall).
pub async fn unlink_tickets(db: &PgPool, actor: &User, id: i64, other: i64) -> AppResult<()> {
    let removed = sqlx::query("DELETE FROM ticket_links WHERE a_id = $1 AND b_id = $2")
        .bind(id.min(other))
        .bind(id.max(other))
        .execute(db)
        .await?
        .rows_affected();
    if removed > 0 {
        let mut tx = db.begin().await?;
        for (a, b) in [(id, other), (other, id)] {
            let ev = AuditEvent::Link { ticket_id: b, relation: "related".into(), added: false };
            insert_audit(&mut tx, a, Some(actor.id), "web", &[ev]).await?;
        }
        tx.commit().await?;
        return Ok(());
    }
    for (incident, problem) in [(id, other), (other, id)] {
        let t = get_ticket(db, incident).await?;
        if t.problem_id == Some(problem) {
            let changes = TicketChanges { problem_id: Some(None), ..Default::default() };
            update_ticket(db, actor, incident, changes, None, "web").await?;
            return Ok(());
        }
    }
    Err(AppError::invalid(format!("Ticket #{other} ist nicht verknüpft")))
}

// ---------------------------------------------------------------------------
// Eigener Status
// ---------------------------------------------------------------------------

/// Eigenen Status setzen bzw. entfernen (`None` = nur Grundart) und protokollieren.
pub async fn set_substatus(db: &PgPool, actor: &User, ticket_id: i64, substatus: Option<String>) -> AppResult<()> {
    let mut tx = db.begin().await?;
    let old: Option<Option<String>> = sqlx::query_scalar("SELECT substatus FROM tickets WHERE id = $1 FOR UPDATE")
        .bind(ticket_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(old) = old else { return Err(AppError::NotFound) };
    if old == substatus {
        return Ok(());
    }
    sqlx::query("UPDATE tickets SET substatus = $2, updated_at = now() WHERE id = $1")
        .bind(ticket_id)
        .bind(&substatus)
        .execute(&mut *tx)
        .await?;
    let ev = AuditEvent::Change {
        field: "substatus".into(),
        previous_value: old.map(Value::from).unwrap_or(Value::Null),
        value: substatus.map(Value::from).unwrap_or(Value::Null),
    };
    insert_audit(&mut tx, ticket_id, Some(actor.id), "web", &[ev]).await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Auto-Modus: Ticket übernehmen
// ---------------------------------------------------------------------------

/// Weist ein noch freies Ticket atomar dem Agenten zu (zwei Agenten im Auto-Modus
/// können also nie dasselbe Ticket bekommen). `false`, wenn es inzwischen vergeben ist.
pub async fn claim_ticket(db: &PgPool, actor: &User, ticket_id: i64) -> AppResult<bool> {
    let mut tx = db.begin().await?;
    let claimed: Option<i64> = sqlx::query_scalar(
        "UPDATE tickets SET assignee_id = $1, updated_at = now()
         WHERE id = $2 AND assignee_id IS NULL AND status NOT IN ('solved', 'closed') RETURNING id",
    )
    .bind(actor.id)
    .bind(ticket_id)
    .fetch_optional(&mut *tx)
    .await?;
    if claimed.is_none() {
        return Ok(false);
    }
    let ev = AuditEvent::Change { field: "assignee_id".into(), previous_value: Value::Null, value: Value::from(actor.id) };
    insert_audit(&mut tx, ticket_id, Some(actor.id), "auto", &[ev]).await?;
    tx.commit().await?;
    Ok(true)
}

/// Zuweisung wieder lösen (Überspringen im Auto-Modus), nur wenn sie noch beim Agenten liegt.
pub async fn release_ticket(db: &PgPool, actor: &User, ticket_id: i64) -> AppResult<()> {
    let mut tx = db.begin().await?;
    let released: Option<i64> = sqlx::query_scalar(
        "UPDATE tickets SET assignee_id = NULL, updated_at = now() WHERE id = $1 AND assignee_id = $2 RETURNING id",
    )
    .bind(ticket_id)
    .bind(actor.id)
    .fetch_optional(&mut *tx)
    .await?;
    if released.is_some() {
        let ev = AuditEvent::Change { field: "assignee_id".into(), previous_value: Value::from(actor.id), value: Value::Null };
        insert_audit(&mut tx, ticket_id, Some(actor.id), "auto", &[ev]).await?;
    }
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Löschen von Kommentaren und Anhängen
// ---------------------------------------------------------------------------

/// Löschen dürfen Admins sowie der Verfasser bzw. wer die Datei hochgeladen hat.
/// Geschlossene Tickets bleiben unverändert.
fn may_delete(actor: &User, owner: Option<i64>, ticket: &Ticket) -> AppResult<()> {
    if ticket.status == "closed" {
        return Err(AppError::invalid("Das Ticket ist geschlossen und kann nicht mehr geändert werden"));
    }
    if actor.is_admin() || (actor.is_agent() && owner == Some(actor.id)) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

/// Kommentar samt seiner Anhänge löschen; im Ereignisprotokoll bleibt vermerkt, wer
/// wann was gelöscht hat (ohne den Inhalt).
pub async fn delete_comment(db: &PgPool, actor: &User, ticket_id: i64, comment_id: i64) -> AppResult<()> {
    let ticket = get_ticket(db, ticket_id).await?;
    let row: Option<(i64, bool, String)> = sqlx::query_as(
        "SELECT c.author_id, c.public, u.name FROM ticket_comments c JOIN users u ON u.id = c.author_id WHERE c.id = $1 AND c.ticket_id = $2",
    )
    .bind(comment_id)
    .bind(ticket_id)
    .fetch_optional(db)
    .await?;
    let Some((author_id, public, author)) = row else { return Err(AppError::NotFound) };
    may_delete(actor, Some(author_id), &ticket)?;
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM ticket_comments WHERE id = $1").bind(comment_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE tickets SET updated_at = now() WHERE id = $1").bind(ticket_id).execute(&mut *tx).await?;
    let what = if public { "Öffentlicher Kommentar" } else { "Interne Notiz" };
    let ev = AuditEvent::Deleted { item: what.into(), name: format!("von {author}") };
    insert_audit(&mut tx, ticket_id, Some(actor.id), "web", &[ev]).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn delete_attachment(db: &PgPool, actor: &User, ticket_id: i64, attachment_id: i64) -> AppResult<()> {
    let ticket = get_ticket(db, ticket_id).await?;
    let row: Option<(String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT a.filename, a.uploaded_by, c.author_id FROM attachments a LEFT JOIN ticket_comments c ON c.id = a.comment_id
         WHERE a.id = $1 AND a.ticket_id = $2",
    )
    .bind(attachment_id)
    .bind(ticket_id)
    .fetch_optional(db)
    .await?;
    let Some((filename, uploaded_by, comment_author)) = row else { return Err(AppError::NotFound) };
    // Eigene Uploads und Anhänge an eigenen Kommentaren.
    let owner = if uploaded_by == Some(actor.id) { uploaded_by } else { comment_author };
    may_delete(actor, owner, &ticket)?;
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM attachments WHERE id = $1").bind(attachment_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE tickets SET updated_at = now() WHERE id = $1").bind(ticket_id).execute(&mut *tx).await?;
    let ev = AuditEvent::Deleted { item: "Anhang".into(), name: filename };
    insert_audit(&mut tx, ticket_id, Some(actor.id), "web", &[ev]).await?;
    tx.commit().await?;
    Ok(())
}

/// Löst alle offenen Vorfälle eines gelösten Problems (wie bei Zendesk) und
/// informiert deren Anfragende mit dem Lösungstext des Problems.
async fn solve_incidents(db: &PgPool, actor: &User, problem: &Ticket, note: Option<String>) -> AppResult<()> {
    let incidents: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM tickets WHERE problem_id = $1 AND status NOT IN ('solved', 'closed') ORDER BY id",
    )
    .bind(problem.id)
    .fetch_all(db)
    .await?;
    let body = note.unwrap_or_else(|| "Das zugrunde liegende Problem wurde behoben. Ihre Anfrage ist damit gelöst.".into());
    for id in incidents {
        let changes = TicketChanges {
            status: Some("solved".into()),
            comment: Some(CommentChange { body: body.clone(), public: true, author_id: None }),
            ..Default::default()
        };
        // Einzelne Vorfälle (z. B. fehlende Pflichtfelder) blockieren die anderen nicht.
        if let Err(e) = Box::pin(update_ticket_with(db, actor, id, changes, None, "problem")).await {
            tracing::warn!(ticket = id, problem = problem.id, "Vorfall nicht gelöst: {}", e.public_message());
            let note = TicketChanges {
                comment: Some(CommentChange {
                    body: format!(
                        "Problem #{} wurde gelöst, dieser Vorfall konnte nicht automatisch gelöst werden: {}",
                        problem.id,
                        e.public_message()
                    ),
                    public: false,
                    author_id: None,
                }),
                ..Default::default()
            };
            Box::pin(update_ticket_with(db, actor, id, note, None, "problem")).await.ok();
        }
    }
    Ok(())
}

/// Führt ein Ticket in ein anderes zusammen (wie bei Zendesk): Der Verlauf der
/// Quelle wird als interne Notiz ins Ziel übernommen, die Quelle wird geschlossen.
pub async fn merge_tickets(db: &PgPool, actor: &User, source_id: i64, target_id: i64) -> AppResult<Ticket> {
    if source_id == target_id {
        return Err(AppError::invalid("Ein Ticket kann nicht mit sich selbst zusammengeführt werden"));
    }
    let source = get_ticket(db, source_id).await?;
    let target = get_ticket(db, target_id)
        .await
        .map_err(|_| AppError::invalid(format!("Zielticket #{target_id} existiert nicht")))?;
    if source.status == "closed" || target.status == "closed" {
        return Err(AppError::invalid("Geschlossene Tickets können nicht zusammengeführt werden"));
    }

    let history: Vec<(String, String, bool, DateTime<Utc>)> = sqlx::query_as(
        "SELECT u.name, c.body, c.public, c.created_at FROM ticket_comments c JOIN users u ON u.id = c.author_id
         WHERE c.ticket_id = $1 ORDER BY c.created_at, c.id",
    )
    .bind(source_id)
    .fetch_all(db)
    .await?;
    let mut note = format!("Ticket #{source_id} „{}“ wurde hierher zusammengeführt.\n", source.subject);
    for (author, body, public, at) in history {
        let kind = if public { "" } else { " (intern)" };
        let local = at.with_timezone(&chrono::Local).format("%d.%m.%Y %H:%M");
        note.push_str(&format!("\n— {author}{kind}, {local}:\n{body}\n"));
    }

    update_ticket(
        db,
        actor,
        source_id,
        TicketChanges {
            status: Some("closed".into()),
            additional_tags: Some(vec!["zusammengefuehrt".into()]),
            comment: Some(CommentChange {
                body: format!("Zusammengeführt in Ticket #{target_id} „{}“.", target.subject),
                public: false,
                author_id: None,
            }),
            ..Default::default()
        },
        None,
        "merge",
    )
    .await?;
    update_ticket(
        db,
        actor,
        target_id,
        TicketChanges {
            additional_tags: Some(source.tags.clone()),
            comment: Some(CommentChange { body: note, public: false, author_id: None }),
            ..Default::default()
        },
        None,
        "merge",
    )
    .await
}

/// Führt einen doppelt angelegten Kunden in einen anderen zusammen: Tickets und
/// Kommentare wandern zum Ziel, fehlende Kontaktdaten werden übernommen, die Quelle gelöscht.
pub async fn merge_users(db: &PgPool, actor: &User, source_id: i64, target_id: i64) -> AppResult<User> {
    if source_id == target_id {
        return Err(AppError::invalid("Ein Kunde kann nicht mit sich selbst zusammengeführt werden"));
    }
    let mut tx = db.begin().await?;
    let source = get_user(&mut tx, source_id).await?.ok_or(AppError::NotFound)?;
    let target = get_user(&mut tx, target_id)
        .await?
        .ok_or_else(|| AppError::invalid("Zielkunde existiert nicht"))?;
    if source.is_agent() || target.is_agent() {
        return Err(AppError::invalid("Nur Endkunden können zusammengeführt werden"));
    }
    if !actor.is_agent() {
        return Err(AppError::Forbidden);
    }

    let moved: Vec<i64> = sqlx::query_scalar("SELECT id FROM tickets WHERE requester_id = $1")
        .bind(source_id)
        .fetch_all(&mut *tx)
        .await?;
    sqlx::query("UPDATE tickets SET requester_id = $2, organization_id = COALESCE(organization_id, $3), updated_at = now() WHERE requester_id = $1")
        .bind(source_id)
        .bind(target_id)
        .bind(target.organization_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE ticket_comments SET author_id = $2 WHERE author_id = $1")
        .bind(source_id)
        .bind(target_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE tickets SET submitter_id = $2 WHERE submitter_id = $1")
        .bind(source_id)
        .bind(target_id)
        .execute(&mut *tx)
        .await?;
    for ticket_id in &moved {
        let events = vec![AuditEvent::Change {
            field: "requester_id".into(),
            previous_value: json!(source_id),
            value: json!(target_id),
        }];
        insert_audit(&mut tx, *ticket_id, Some(actor.id), "merge", &events).await?;
    }
    // Quelle zuerst löschen, damit E-Mail/Telefon (eindeutig) übernommen werden können.
    sqlx::query("DELETE FROM users WHERE id = $1").bind(source_id).execute(&mut *tx).await?;
    let merged: User = sqlx::query_as(
        "UPDATE users SET email = COALESCE(email, $2), phone = COALESCE(phone, $3),
            organization_id = COALESCE(organization_id, $4),
            password_hash = COALESCE(password_hash, $5), updated_at = now()
         WHERE id = $1 RETURNING *",
    )
    .bind(target_id)
    .bind(&source.email)
    .bind(&source.phone)
    .bind(source.organization_id)
    .bind(&source.password_hash)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(merged)
}

/// Kundenbewertung (CSAT) für ein gelöstes Ticket; nur durch den Anfragenden.
pub async fn rate_ticket(db: &PgPool, customer: &User, id: i64, score: &str, comment: &str) -> AppResult<Ticket> {
    if !matches!(score, "good" | "bad") {
        return Err(AppError::invalid("Ungültige Bewertung"));
    }
    let mut tx = db.begin().await?;
    let before: Ticket = sqlx::query_as("SELECT * FROM tickets WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;
    if before.requester_id != customer.id {
        return Err(AppError::NotFound);
    }
    if !matches!(before.status.as_str(), "solved" | "closed") {
        return Err(AppError::invalid("Bewerten ist erst möglich, wenn die Anfrage gelöst ist"));
    }
    let comment = comment.trim();
    let t: Ticket = sqlx::query_as(
        "UPDATE tickets SET satisfaction_score = $2, satisfaction_comment = NULLIF($3, ''), satisfaction_at = now()
         WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .bind(score)
    .bind(comment)
    .fetch_one(&mut *tx)
    .await?;
    let events = vec![AuditEvent::Change {
        field: "satisfaction_score".into(),
        previous_value: json!(before.satisfaction_score),
        value: json!(score),
    }];
    insert_audit(&mut tx, id, Some(customer.id), "portal", &events).await?;
    tx.commit().await?;
    Ok(t)
}

/// Obergrenzen für den Upload-Körper (größtmögliche Einstellungen).
pub const UPLOAD_LIMIT_BYTES: usize = 10 * 25 * 1024 * 1024 + 1024 * 1024;

pub fn max_attachments() -> usize {
    crate::config::int("attachments.max_count") as usize
}

pub fn max_attachment_mb() -> usize {
    crate::config::int("attachments.max_mb") as usize
}

pub struct Upload {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

/// Lädt Dateien als neuen Kommentar hoch (läuft durch die normale Ticketlogik:
/// Trigger, Wiedereröffnung bei Kundenantwort, Audit).
pub async fn attach_files(
    db: &PgPool,
    actor: &User,
    ticket_id: i64,
    public: bool,
    note: &str,
    files: Vec<Upload>,
    via: &str,
) -> AppResult<()> {
    let files: Vec<Upload> = files.into_iter().filter(|f| !f.data.is_empty()).collect();
    if files.is_empty() {
        return Err(AppError::invalid("Bitte mindestens eine Datei auswählen"));
    }
    let (max_count, max_mb) = (max_attachments(), max_attachment_mb());
    if files.len() > max_count {
        return Err(AppError::invalid(format!("Höchstens {max_count} Dateien auf einmal")));
    }
    if let Some(f) = files.iter().find(|f| f.data.len() > max_mb * 1024 * 1024) {
        return Err(AppError::invalid(format!("„{}“ ist größer als {max_mb} MB", f.filename)));
    }
    // Die Dateien selbst erscheinen als Links unter dem Kommentar.
    let body = match (note.trim(), files.len()) {
        ("", 1) => "Datei angehängt".to_string(),
        ("", n) => format!("{n} Dateien angehängt"),
        (text, _) => text.to_string(),
    };
    update_ticket(
        db,
        actor,
        ticket_id,
        TicketChanges { comment: Some(CommentChange { body, public, author_id: None }), ..Default::default() },
        None,
        via,
    )
    .await?;
    let comment_id: i64 = sqlx::query_scalar(
        "SELECT id FROM ticket_comments WHERE ticket_id = $1 AND author_id = $2 ORDER BY id DESC LIMIT 1",
    )
    .bind(ticket_id)
    .bind(actor.id)
    .fetch_one(db)
    .await?;
    for f in files {
        sqlx::query(
            "INSERT INTO attachments (ticket_id, comment_id, filename, content_type, size, data, uploaded_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(ticket_id)
        .bind(comment_id)
        .bind(&f.filename)
        .bind(&f.content_type)
        .bind(f.data.len() as i64)
        .bind(&f.data)
        .bind(actor.id)
        .execute(db)
        .await?;
    }
    Ok(())
}

/// DSGVO Art. 15/20: alle personenbezogenen Daten eines Kunden als JSON.
pub async fn export_user_data(db: &PgPool, id: i64) -> AppResult<Value> {
    let user: User = sqlx::query_as("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)?;
    let org: Option<String> = sqlx::query_scalar("SELECT name FROM organizations WHERE id = $1")
        .bind(user.organization_id)
        .fetch_optional(db)
        .await?;
    let tickets: Vec<Ticket> = sqlx::query_as("SELECT * FROM tickets WHERE requester_id = $1 ORDER BY id")
        .bind(id)
        .fetch_all(db)
        .await?;
    let mut out = Vec::new();
    for t in tickets {
        let comments: Vec<(String, String, bool, DateTime<Utc>)> = sqlx::query_as(
            "SELECT u.name, c.body, c.public, c.created_at FROM ticket_comments c JOIN users u ON u.id = c.author_id
             WHERE c.ticket_id = $1 ORDER BY c.id",
        )
        .bind(t.id)
        .fetch_all(db)
        .await?;
        let files: Vec<(String, i64)> = sqlx::query_as("SELECT filename, size FROM attachments WHERE ticket_id = $1 ORDER BY id")
            .bind(t.id)
            .fetch_all(db)
            .await?;
        out.push(json!({
            "id": t.id, "subject": t.subject, "status": t.status, "channel": t.channel,
            "created_at": t.created_at, "custom_fields": t.custom_fields,
            "satisfaction": t.satisfaction_score, "satisfaction_comment": t.satisfaction_comment,
            "comments": comments.into_iter().map(|(author, body, public, at)| json!({
                "author": author, "body": body, "public": public, "created_at": at
            })).collect::<Vec<_>>(),
            "attachments": files.into_iter().map(|(name, size)| json!({ "filename": name, "size": size })).collect::<Vec<_>>(),
        }));
    }
    Ok(json!({
        "exported_at": Utc::now(),
        "person": {
            "id": user.id, "name": user.name, "email": user.email, "phone": user.phone,
            "organization": org, "created_at": user.created_at, "portal_access": user.password_hash.is_some(),
        },
        "tickets": out,
    }))
}

/// DSGVO Art. 17: Kunde anonymisieren. Tickets bleiben für Statistik und Nachvollziehbarkeit
/// erhalten, alle personenbezogenen Inhalte des Kunden werden entfernt.
pub async fn anonymize_user(db: &PgPool, actor: &User, id: i64) -> AppResult<()> {
    if !actor.is_admin() {
        return Err(AppError::Forbidden);
    }
    let mut tx = db.begin().await?;
    let user = get_user(&mut tx, id).await?.ok_or(AppError::NotFound)?;
    if user.is_agent() {
        return Err(AppError::invalid("Agenten werden über die Agentenverwaltung deaktiviert"));
    }
    const REMOVED: &str = "[Inhalt auf Kundenwunsch gelöscht]";
    let tickets: Vec<i64> = sqlx::query_scalar("SELECT id FROM tickets WHERE requester_id = $1")
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
    sqlx::query("UPDATE ticket_comments SET body = $2 WHERE author_id = $1").bind(id).bind(REMOVED).execute(&mut *tx).await?;
    sqlx::query("UPDATE tickets SET description = $2, satisfaction_comment = NULL, custom_fields = '{}' WHERE requester_id = $1")
        .bind(id)
        .bind(REMOVED)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM attachments WHERE uploaded_by = $1").bind(id).execute(&mut *tx).await?;
    if let Some(email) = &user.email {
        sqlx::query("DELETE FROM email_outbox WHERE lower(to_email) = lower($1)").bind(email).execute(&mut *tx).await?;
    }
    for table in ["sessions", "api_tokens", "password_resets"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE user_id = $1")).bind(id).execute(&mut *tx).await?;
    }
    sqlx::query(
        "UPDATE users SET name = $2, email = NULL, phone = NULL, password_hash = NULL, organization_id = NULL,
            active = FALSE, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(format!("Gelöschter Kunde #{id}"))
    .execute(&mut *tx)
    .await?;
    let event = vec![AuditEvent::RuleApplied { kind: "gdpr".into(), id, title: "Kundendaten anonymisiert".into() }];
    for ticket_id in tickets {
        insert_audit(&mut tx, ticket_id, Some(actor.id), "gdpr", &event).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn get_ticket(db: &PgPool, id: i64) -> AppResult<Ticket> {
    sqlx::query_as("SELECT * FROM tickets WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

// ---------------------------------------------------------------------------
// Views & Suche
// ---------------------------------------------------------------------------

/// Kandidaten für die View-Auswertung: alles außer lange geschlossenen Tickets.
pub async fn candidate_tickets(db: &PgPool) -> AppResult<Vec<Ticket>> {
    Ok(sqlx::query_as(
        "SELECT * FROM tickets WHERE status <> 'closed' OR updated_at > now() - interval '90 days'
         ORDER BY id DESC LIMIT 20000",
    )
    .fetch_all(db)
    .await?)
}

pub fn filter_view(view: &View, tickets: &[Ticket], me: &User, now: DateTime<Utc>) -> Vec<Ticket> {
    let ctx = EvalCtx::passive(Some(me), now);
    let mut out: Vec<Ticket> =
        tickets.iter().filter(|t| view.conditions.0.matches(t, &ctx)).cloned().collect();
    sort_tickets(&mut out, &view.execution.0);
    out
}

pub fn count_view(view: &View, tickets: &[Ticket], me: &User, now: DateTime<Utc>) -> usize {
    let ctx = EvalCtx::passive(Some(me), now);
    tickets.iter().filter(|t| view.conditions.0.matches(t, &ctx)).count()
}

fn sort_tickets(tickets: &mut [Ticket], ex: &ViewExecution) {
    let prio = |t: &Ticket| t.priority.as_deref().and_then(priority_rank);
    let status = |t: &Ticket| status_rank(&t.status);
    tickets.sort_by(|a, b| {
        let ord = match ex.sort_by.as_str() {
            "updated_at" => a.updated_at.cmp(&b.updated_at),
            "priority" => prio(a).cmp(&prio(b)),
            "status" => status(a).cmp(&status(b)),
            "id" => a.id.cmp(&b.id),
            "due_at" => a.due_at.cmp(&b.due_at),
            _ => a.created_at.cmp(&b.created_at),
        };
        if ex.sort_order == "asc" { ord } else { ord.reverse() }
    });
}

fn like_pattern(s: &str) -> String {
    format!("%{}%", s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

/// Ticketsuche mit Zendesk-ähnlicher Syntax:
/// `drucker status:open priority:urgent tag:vip assignee:me requester:max@acme.de group:support type:incident channel:phone`
pub async fn search_tickets(db: &PgPool, query: &str, me: &User) -> AppResult<Vec<Ticket>> {
    let mut tickets = crate::search::tickets(db, query, me, "relevance").await?;
    tickets.truncate(crate::search::LIMIT);
    Ok(tickets)
}

/// Personensuche (Name, E-Mail, Telefon). `customers_only`: nur Endkunden, keine Mitarbeitenden.
pub async fn search_users(db: &PgPool, query: &str, limit: i64, customers_only: bool) -> AppResult<Vec<User>> {
    let query = query.trim();
    // Telefon-Treffer erst ab 4 Ziffern, sonst passt z. B. "#5" auf jede Nummer mit einer 5.
    let phone = normalize_phone(query);
    let phone = if phone.chars().filter(char::is_ascii_digit).count() >= 4 { phone } else { String::new() };
    Ok(sqlx::query_as(
        "SELECT * FROM users
         WHERE (name ILIKE $1 OR email ILIKE $1 OR ($2 <> '' AND phone LIKE $3))
           AND (role = 'end-user' OR NOT $5)
           AND email IS DISTINCT FROM 'system@automation.invalid'
         ORDER BY role <> 'end-user', name LIMIT $4",
    )
    .bind(like_pattern(query))
    .bind(&phone)
    .bind(format!("%{phone}%"))
    .bind(limit)
    .bind(customers_only)
    .fetch_all(db)
    .await?)
}

// ---------------------------------------------------------------------------
// Benutzer & Organisationen
// ---------------------------------------------------------------------------

fn default_country_code() -> String {
    crate::config::int("phone.country_code").to_string()
}

/// Bringt Telefonnummern in ein einheitliches internationales Format, damit ein
/// Anrufer unabhängig von der Schreibweise wiedererkannt wird:
/// `0171 999-888`, `+49 (0) 171 999888`, `0049171999888` → `+49171999888`.
pub fn normalize_phone(s: &str) -> String {
    normalize_phone_with(s, &default_country_code())
}

fn normalize_phone_with(s: &str, country: &str) -> String {
    let s = s.replace("(0)", "");
    let international = s.trim_start().starts_with('+');
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return String::new();
    }
    if international {
        return format!("+{digits}");
    }
    if let Some(rest) = digits.strip_prefix("00") {
        return format!("+{rest}");
    }
    match digits.strip_prefix('0') {
        Some(rest) if !rest.is_empty() => format!("+{country}{rest}"),
        // Ohne führende 0 (z. B. Durchwahl oder Teil einer Nummer) unverändert.
        _ => digits,
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_phone_with;

    #[test]
    fn phone_variants_are_equal() {
        for input in ["+49 (0) 171 / 999-888", "+49 171 999888", "0171 999888", "0049171999888"] {
            assert_eq!(normalize_phone_with(input, "49"), "+49171999888", "{input}");
        }
        assert_eq!(normalize_phone_with("0041 44 123", "49"), "+4144123");
        assert_eq!(normalize_phone_with("1234", "49"), "1234");
        assert_eq!(normalize_phone_with("abc", "49"), "");
    }
}

fn clean(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

async fn org_for_email(db: &PgPool, email: Option<&str>) -> AppResult<Option<i64>> {
    let Some(domain) = email.and_then(|e| e.rsplit_once('@')).map(|(_, d)| d.to_lowercase()) else {
        return Ok(None);
    };
    Ok(sqlx::query_scalar("SELECT id FROM organizations WHERE $1 = ANY(domain_names) ORDER BY id LIMIT 1")
        .bind(domain)
        .fetch_optional(db)
        .await?)
}

/// Sucht einen Anfragenden per E-Mail oder Telefonnummer und legt ihn sonst an.
pub async fn find_or_create_requester(
    db: &PgPool,
    name: &str,
    email: Option<&str>,
    phone: Option<&str>,
) -> AppResult<User> {
    let email = clean(email).map(|e| e.to_lowercase());
    let phone = clean(phone).map(|p| normalize_phone(&p)).filter(|p| !p.is_empty());
    if let Some(e) = &email {
        if let Some(u) = sqlx::query_as("SELECT * FROM users WHERE lower(email) = $1")
            .bind(e)
            .fetch_optional(db)
            .await?
        {
            return Ok(u);
        }
    }
    if let Some(p) = &phone {
        if let Some(u) = sqlx::query_as("SELECT * FROM users WHERE phone = $1 ORDER BY id LIMIT 1")
            .bind(p)
            .fetch_optional(db)
            .await?
        {
            return Ok(u);
        }
    }
    let display = clean(Some(name))
        .or_else(|| email.clone())
        .or_else(|| phone.clone())
        .ok_or_else(|| AppError::invalid("Name, E-Mail oder Telefonnummer des Anfragenden angeben"))?;
    if let Some(e) = &email {
        if !e.contains('@') {
            return Err(AppError::invalid("Ungültige E-Mail-Adresse"));
        }
    }
    let org = org_for_email(db, email.as_deref()).await?;
    Ok(sqlx::query_as(
        "INSERT INTO users (name, email, phone, role, organization_id) VALUES ($1, $2, $3, 'end-user', $4) RETURNING *",
    )
    .bind(display)
    .bind(&email)
    .bind(&phone)
    .bind(org)
    .fetch_one(db)
    .await?)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserInput {
    pub name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub organization_id: Option<Option<i64>>,
    pub active: Option<bool>,
    pub password: Option<String>,
    pub availability: Option<String>,
    pub capacity: Option<i32>,
}

pub async fn create_user(db: &PgPool, actor: &User, input: UserInput) -> AppResult<User> {
    let name = clean(input.name.as_deref()).ok_or_else(|| AppError::invalid("Name darf nicht leer sein"))?;
    let role = clean(input.role.as_deref()).unwrap_or_else(|| "end-user".into());
    check_enum(Some(&role), &ROLES, "Rolle")?;
    if role != "end-user" && !actor.is_admin() {
        return Err(AppError::Forbidden);
    }
    let email = clean(input.email.as_deref()).map(|e| e.to_lowercase());
    if email.as_deref().is_some_and(|e| !e.contains('@')) {
        return Err(AppError::invalid("Ungültige E-Mail-Adresse"));
    }
    if role != "end-user" && email.is_none() {
        return Err(AppError::invalid("Agenten benötigen eine E-Mail-Adresse"));
    }
    let phone = clean(input.phone.as_deref()).map(|p| normalize_phone(&p)).filter(|p| !p.is_empty());
    let password_hash = match clean(input.password.as_deref()) {
        Some(pw) if pw.len() < 8 => return Err(AppError::invalid("Passwort muss mindestens 8 Zeichen haben")),
        Some(pw) => Some(auth::hash_password(&pw)?),
        None => None,
    };
    let org = match input.organization_id {
        Some(org) => org,
        None => org_for_email(db, email.as_deref()).await?,
    };
    Ok(sqlx::query_as(
        "INSERT INTO users (name, email, phone, role, organization_id, password_hash, active)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
    )
    .bind(name)
    .bind(email)
    .bind(phone)
    .bind(role)
    .bind(org)
    .bind(password_hash)
    .bind(input.active.unwrap_or(true))
    .fetch_one(db)
    .await?)
}

pub async fn update_user(db: &PgPool, actor: &User, id: i64, input: UserInput) -> AppResult<User> {
    let mut u: User = sqlx::query_as("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)?;

    let is_self = actor.id == u.id;
    if !actor.is_admin() {
        // Agenten dürfen Endkunden pflegen und ihr eigenes Profil, sonst nichts.
        if (u.is_agent() && !is_self) || input.role.is_some() || input.active.is_some() || input.capacity.is_some() {
            return Err(AppError::Forbidden);
        }
    }
    if let Some(availability) = &input.availability {
        check_enum(Some(availability), &AVAILABILITIES, "Status")?;
        u.availability = availability.clone();
    }
    if let Some(capacity) = input.capacity {
        if !(0..=500).contains(&capacity) {
            return Err(AppError::invalid("Kapazität muss zwischen 0 und 500 liegen"));
        }
        u.capacity = capacity;
    }
    if let Some(name) = &input.name {
        u.name = clean(Some(name)).ok_or_else(|| AppError::invalid("Name darf nicht leer sein"))?;
    }
    if let Some(email) = &input.email {
        u.email = clean(Some(email)).map(|e| e.to_lowercase());
        if u.email.as_deref().is_some_and(|e| !e.contains('@')) {
            return Err(AppError::invalid("Ungültige E-Mail-Adresse"));
        }
    }
    if let Some(phone) = &input.phone {
        u.phone = clean(Some(phone)).map(|p| normalize_phone(&p)).filter(|p| !p.is_empty());
    }
    if let Some(role) = &input.role {
        check_enum(Some(role), &ROLES, "Rolle")?;
        if is_self && role != "admin" {
            return Err(AppError::invalid("Du kannst dir die Admin-Rolle nicht selbst entziehen"));
        }
        u.role = role.clone();
    }
    if let Some(org) = input.organization_id {
        u.organization_id = org;
    }
    if let Some(active) = input.active {
        if is_self && !active {
            return Err(AppError::invalid("Du kannst dich nicht selbst deaktivieren"));
        }
        u.active = active;
    }
    if u.is_agent() && u.email.is_none() {
        return Err(AppError::invalid("Agenten benötigen eine E-Mail-Adresse"));
    }
    if let Some(pw) = clean(input.password.as_deref()) {
        // Agenten dürfen Kunden einen Portalzugang einrichten.
        if !(actor.is_admin() || is_self || (actor.is_agent() && !u.is_agent())) {
            return Err(AppError::Forbidden);
        }
        if pw.len() < 8 {
            return Err(AppError::invalid("Passwort muss mindestens 8 Zeichen haben"));
        }
        u.password_hash = Some(auth::hash_password(&pw)?);
    }
    Ok(sqlx::query_as(
        "UPDATE users SET name = $2, email = $3, phone = $4, role = $5, organization_id = $6,
            active = $7, password_hash = $8, availability = $9, capacity = $10, updated_at = now()
         WHERE id = $1 RETURNING *",
    )
    .bind(u.id)
    .bind(&u.name)
    .bind(&u.email)
    .bind(&u.phone)
    .bind(&u.role)
    .bind(u.organization_id)
    .bind(u.active)
    .bind(&u.password_hash)
    .bind(&u.availability)
    .bind(u.capacity)
    .fetch_one(db)
    .await?)
}

/// Setzt den Agentenstatus; geht ein Agent online, wird die Warteschlange verteilt.
pub async fn set_availability(db: &PgPool, user_id: i64, availability: &str) -> AppResult<()> {
    check_enum(Some(availability), &AVAILABILITIES, "Status")?;
    sqlx::query("UPDATE users SET availability = $2 WHERE id = $1")
        .bind(user_id)
        .bind(availability)
        .execute(db)
        .await?;
    if availability == "online" {
        routing::process_queue(db).await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OrgInput {
    pub name: Option<String>,
    pub domain_names: Option<Vec<String>>,
    pub notes: Option<String>,
}

fn clean_domains(domains: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in domains {
        let d = d.trim().trim_start_matches('@').to_lowercase();
        if !d.is_empty() && !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

pub async fn create_org(db: &PgPool, input: OrgInput) -> AppResult<Organization> {
    let name = clean(input.name.as_deref()).ok_or_else(|| AppError::invalid("Name darf nicht leer sein"))?;
    Ok(sqlx::query_as(
        "INSERT INTO organizations (name, domain_names, notes) VALUES ($1, $2, $3) RETURNING *",
    )
    .bind(name)
    .bind(clean_domains(&input.domain_names.unwrap_or_default()))
    .bind(input.notes.unwrap_or_default())
    .fetch_one(db)
    .await?)
}

pub async fn update_org(db: &PgPool, id: i64, input: OrgInput) -> AppResult<Organization> {
    let mut org: Organization = sqlx::query_as("SELECT * FROM organizations WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)?;
    if let Some(name) = &input.name {
        org.name = clean(Some(name)).ok_or_else(|| AppError::invalid("Name darf nicht leer sein"))?;
    }
    if let Some(domains) = &input.domain_names {
        org.domain_names = clean_domains(domains);
    }
    if let Some(notes) = input.notes {
        org.notes = notes;
    }
    Ok(sqlx::query_as(
        "UPDATE organizations SET name = $2, domain_names = $3, notes = $4, updated_at = now() WHERE id = $1 RETURNING *",
    )
    .bind(org.id)
    .bind(&org.name)
    .bind(&org.domain_names)
    .bind(&org.notes)
    .fetch_one(db)
    .await?)
}

pub async fn create_group(db: &PgPool, name: &str, description: &str) -> AppResult<Group> {
    let name = clean(Some(name)).ok_or_else(|| AppError::invalid("Name darf nicht leer sein"))?;
    Ok(sqlx::query_as("INSERT INTO agent_groups (name, description) VALUES ($1, $2) RETURNING *")
        .bind(name)
        .bind(description.trim())
        .fetch_one(db)
        .await?)
}

pub async fn ticket_fields(db: &PgPool, only_active: bool) -> AppResult<Vec<TicketField>> {
    Ok(sqlx::query_as("SELECT * FROM ticket_fields WHERE active OR NOT $1 ORDER BY position, id")
        .bind(only_active)
        .fetch_all(db)
        .await?)
}

#[derive(Debug, Clone, Default)]
pub struct FieldInput {
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub options: Vec<String>,
    pub required_on_solve: bool,
    pub position: i32,
    pub active: bool,
}

fn validate_field(input: &FieldInput) -> AppResult<()> {
    if input.label.trim().is_empty() {
        return Err(AppError::invalid("Bezeichnung darf nicht leer sein"));
    }
    check_enum(Some(&input.field_type), &FIELD_TYPES, "Feldtyp")?;
    if input.field_type == "dropdown" && input.options.is_empty() {
        return Err(AppError::invalid("Eine Auswahlliste braucht mindestens eine Option"));
    }
    Ok(())
}

fn clean_options(options: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for o in options.iter().map(|o| o.trim()).filter(|o| !o.is_empty()) {
        if !out.iter().any(|x| x == o) {
            out.push(o.to_string());
        }
    }
    out
}

pub async fn create_field(db: &PgPool, mut input: FieldInput) -> AppResult<TicketField> {
    input.key = input.key.trim().to_lowercase();
    input.options = clean_options(&input.options);
    let key_ok = input.key.len() <= 40
        && input.key.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && input.key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !key_ok {
        return Err(AppError::invalid("Schlüssel: Kleinbuchstaben, Ziffern und _ , beginnend mit einem Buchstaben"));
    }
    validate_field(&input)?;
    Ok(sqlx::query_as(
        "INSERT INTO ticket_fields (key, label, field_type, options, required_on_solve, position, active)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
    )
    .bind(&input.key)
    .bind(input.label.trim())
    .bind(&input.field_type)
    .bind(&input.options)
    .bind(input.required_on_solve)
    .bind(input.position)
    .bind(input.active)
    .fetch_one(db)
    .await?)
}

/// Schlüssel und Typ bleiben fest, damit gespeicherte Werte gültig bleiben.
pub async fn update_field(db: &PgPool, id: i64, mut input: FieldInput) -> AppResult<TicketField> {
    let existing: TicketField = sqlx::query_as("SELECT * FROM ticket_fields WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)?;
    input.field_type = existing.field_type.clone();
    input.options = clean_options(&input.options);
    validate_field(&input)?;
    Ok(sqlx::query_as(
        "UPDATE ticket_fields SET label = $2, options = $3, required_on_solve = $4, position = $5, active = $6
         WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .bind(input.label.trim())
    .bind(&input.options)
    .bind(input.required_on_solve)
    .bind(input.position)
    .bind(input.active)
    .fetch_one(db)
    .await?)
}

/// Formularwerte `cf_<key>` (geändert gegenüber `orig_cf_<key>`) in ein custom_fields-Objekt übersetzen.
pub fn custom_field_changes(
    fields: &[TicketField],
    form: &std::collections::HashMap<String, String>,
    only_changed: bool,
) -> AppResult<Option<Value>> {
    let mut out = serde_json::Map::new();
    for f in fields {
        let Some(new) = form.get(&format!("cf_{}", f.key)) else { continue };
        if only_changed && form.get(&format!("orig_cf_{}", f.key)).map(|s| s.trim()) == Some(new.trim()) {
            continue;
        }
        out.insert(f.key.clone(), f.coerce(new).map_err(AppError::Unprocessable)?);
    }
    Ok((!out.is_empty()).then_some(Value::Object(out)))
}

pub async fn set_group_assignment(db: &PgPool, group_id: i64, assignment: &str) -> AppResult<()> {
    check_enum(Some(assignment), &ASSIGNMENTS, "Verteilung")?;
    let updated = sqlx::query("UPDATE agent_groups SET assignment = $2, updated_at = now() WHERE id = $1")
        .bind(group_id)
        .bind(assignment)
        .execute(db)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    routing::process_queue(db).await?;
    Ok(())
}

pub async fn set_group_memberships(db: &PgPool, user_id: i64, group_ids: &[i64]) -> AppResult<()> {
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM group_memberships WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO group_memberships (group_id, user_id) SELECT unnest($1::bigint[]), $2")
        .bind(group_ids)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
