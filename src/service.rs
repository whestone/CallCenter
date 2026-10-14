//! Geschäftslogik, die von Web-Oberfläche und REST-API gemeinsam genutzt wird.
//!
//! Jede Ticketänderung läuft hier durch: Validierung, eingebaute Zendesk-Regeln
//! (new → open bei Zuweisung, Wiedereröffnung bei Kundenantwort …), Triggers,
//! SLA-Zuordnung und ein Audit-Eintrag mit allen Änderungen.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

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
        ("priority", json!(t.priority)),
        ("type", json!(t.ticket_type)),
        ("requester_id", json!(t.requester_id)),
        ("assignee_id", json!(t.assignee_id)),
        ("group_id", json!(t.group_id)),
        ("organization_id", json!(t.organization_id)),
        ("tags", json!(t.tags)),
        ("sla_policy_id", json!(t.sla_policy_id)),
        ("due_at", json!(t.due_at)),
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
async fn write_effects(
    conn: &mut PgConnection,
    t: &Ticket,
    actor: &User,
    comments: Vec<(i64, CommentInput, String)>,
    notifications: Vec<String>,
    events: &mut Vec<AuditEvent>,
    update_via: &str,
) -> AppResult<()> {
    // Demodaten verschicken keine E-Mails.
    let send_mail = update_via != "seed";
    for (author_id, c, via) in comments {
        let body = render_placeholders(&c.body, t, Some(actor));
        let comment_id = insert_comment(conn, t.id, author_id, &body, c.public, &via).await?;
        events.push(AuditEvent::Comment { comment_id, public: c.public });
        if send_mail && c.public && author_id != t.requester_id {
            let role: Option<String> = sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
                .bind(author_id)
                .fetch_optional(&mut *conn)
                .await?;
            if role.is_some_and(|r| r != "end-user") {
                email::queue_ticket_mail(conn, t, email::TicketMail::Reply { body: &body }).await?;
            }
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
            due_at, first_reply_at, solved_at, created_at, updated_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)
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
    .fetch_one(&mut *tx)
    .await?;

    let mut events = create_events(&t);
    let mut comments = vec![(first_author, first_comment, via.to_string())];
    comments.extend(fx.comments.into_iter().map(|c| (actor.id, c, "trigger".to_string())));
    events.extend(rule_events);
    write_effects(&mut tx, &t, actor, comments, fx.notifications, &mut events, via).await?;
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
            let m: Macro = sqlx::query_as("SELECT * FROM macros WHERE id = $1 AND active")
                .bind(macro_id)
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
            sla_policy_id = $12, due_at = $13, first_reply_at = $14, solved_at = $15, updated_at = $16
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
    .execute(&mut *tx)
    .await?;

    write_effects(&mut tx, &t, actor, comments, std::mem::take(&mut fx.notifications), &mut events, via).await?;
    insert_audit(&mut tx, t.id, Some(actor.id), via, &events).await?;
    webhooks::enqueue(&mut tx, "ticket.updated", &t).await?;
    if t.status == "solved" && before.status != "solved" {
        webhooks::enqueue(&mut tx, "ticket.solved", &t).await?;
    }
    tx.commit().await?;
    Ok(t)
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

pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_ATTACHMENTS: usize = 5;

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
    if files.len() > MAX_ATTACHMENTS {
        return Err(AppError::invalid(format!("Höchstens {MAX_ATTACHMENTS} Dateien auf einmal")));
    }
    if let Some(f) = files.iter().find(|f| f.data.len() > MAX_ATTACHMENT_BYTES) {
        return Err(AppError::invalid(format!("„{}“ ist größer als 10 MB", f.filename)));
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
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("SELECT t.* FROM tickets t WHERE TRUE");
    for token in query.split_whitespace() {
        let (key, value) = token.split_once(':').unwrap_or(("", token));
        match key {
            "status" if value == "unsolved" => {
                qb.push(" AND t.status IN ('new', 'open', 'pending', 'hold')");
            }
            "status" => {
                qb.push(" AND t.status = ").push_bind(value.to_lowercase());
            }
            "priority" => {
                qb.push(" AND t.priority = ").push_bind(value.to_lowercase());
            }
            "type" => {
                qb.push(" AND t.ticket_type = ").push_bind(value.to_lowercase());
            }
            "channel" | "via" => {
                qb.push(" AND t.channel = ").push_bind(value.to_lowercase());
            }
            "tag" | "tags" => {
                qb.push(" AND ").push_bind(value.to_lowercase()).push(" = ANY(t.tags)");
            }
            "assignee" if value == "me" => {
                qb.push(" AND t.assignee_id = ").push_bind(me.id);
            }
            "assignee" if value == "none" => {
                qb.push(" AND t.assignee_id IS NULL");
            }
            "assignee" | "requester" => {
                let col = if key == "assignee" { "t.assignee_id" } else { "t.requester_id" };
                qb.push(format!(" AND {col} IN (SELECT id FROM users WHERE lower(email) = lower("))
                    .push_bind(value.to_string())
                    .push(") OR phone = ")
                    .push_bind(normalize_phone(value))
                    .push(" OR name ILIKE ")
                    .push_bind(like_pattern(value))
                    .push(")");
            }
            "group" => {
                qb.push(" AND t.group_id IN (SELECT id FROM agent_groups WHERE name ILIKE ")
                    .push_bind(like_pattern(value))
                    .push(")");
            }
            "organization" => {
                qb.push(" AND t.organization_id IN (SELECT id FROM organizations WHERE name ILIKE ")
                    .push_bind(like_pattern(value))
                    .push(")");
            }
            _ => {
                let term = token.trim_start_matches('#');
                let pattern = like_pattern(term);
                qb.push(" AND (t.subject ILIKE ")
                    .push_bind(pattern.clone())
                    .push(" OR t.description ILIKE ")
                    .push_bind(pattern.clone())
                    .push(" OR EXISTS (SELECT 1 FROM ticket_comments c WHERE c.ticket_id = t.id AND c.body ILIKE ")
                    .push_bind(pattern)
                    .push(")");
                if let Ok(id) = term.parse::<i64>() {
                    qb.push(" OR t.id = ").push_bind(id);
                }
                qb.push(")");
            }
        }
    }
    qb.push(" ORDER BY t.updated_at DESC LIMIT 200");
    Ok(qb.build_query_as::<Ticket>().fetch_all(db).await?)
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
    std::env::var("DEFAULT_COUNTRY_CODE")
        .ok()
        .map(|c| c.trim().trim_start_matches('+').to_string())
        .filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit()))
        .unwrap_or_else(|| "49".into())
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
