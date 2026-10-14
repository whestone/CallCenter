use std::collections::HashMap;

use askama::Template;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use super::extract::{Form, Path, Query, State};
use super::*;
use crate::{
    auth::WebAgent,
    rules::normalize_tags,
    service::{self, CommentChange, NewTicket, TicketChanges},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/tickets/new", get(new_ticket_page))
        .route("/tickets", post(create_ticket))
        .route("/tickets/{id}", get(show_ticket).post(update_ticket))
        .route("/tickets/{id}/macro", post(apply_macro))
        .route("/tickets/{id}/merge", post(merge_ticket))
        .route("/tickets/{id}/presence", get(presence))
        .route("/tickets/{id}/links", post(add_link))
        .route("/tickets/{id}/links/{other}/delete", post(remove_link))
        .route("/requesters/lookup", get(requester_lookup))
}

// ---------------------------------------------------------------------------
// Ticket-Detail
// ---------------------------------------------------------------------------

struct CommentView {
    author: String,
    initials: String,
    is_agent: bool,
    public: bool,
    /// Formatierter Text (HTML, sicher erzeugt von markdown::render).
    html: String,
    created: String,
    via: String,
    attachments: Vec<super::files::AttachmentView>,
}

struct AuditView {
    created: String,
    author: String,
    lines: Vec<String>,
}

struct SlaView {
    label: &'static str,
    text: String,
    class: &'static str,
}

struct MiniTicket {
    id: i64,
    subject: String,
    status: String,
    status_label: &'static str,
}

struct KeyValue {
    key: String,
    value: String,
}

/// Eingabe für ein eigenes Ticketfeld.
pub struct CfInput {
    pub key: String,
    pub label: String,
    pub ftype: String,
    pub value: String,
    pub opts: Vec<Opt>,
    pub required: bool,
}

pub fn cf_inputs(fields: &[TicketField], values: &Value) -> Vec<CfInput> {
    fields
        .iter()
        .map(|f| {
            let value = f.display(values.get(&f.key));
            let opts = match f.field_type.as_str() {
                "dropdown" => std::iter::once(("", "–"))
                    .chain(f.options.iter().map(|o| (o.as_str(), o.as_str())))
                    .map(|(v, l)| Opt { value: v.into(), label: l.into(), selected: value == v })
                    .collect(),
                "checkbox" => [("", "–"), ("true", "Ja"), ("false", "Nein")]
                    .into_iter()
                    .map(|(v, l)| Opt { value: v.into(), label: l.into(), selected: value == v })
                    .collect(),
                _ => Vec::new(),
            };
            CfInput { key: f.key.clone(), label: f.label.clone(), ftype: f.field_type.clone(), value, opts, required: f.required_on_solve }
        })
        .collect()
}


#[derive(Template)]
#[template(path = "ticket.html")]
struct TicketPage {
    nav: Nav,
    id: i64,
    subject: String,
    status: String,
    status_label: &'static str,
    closed: bool,
    channel_label: &'static str,
    created: String,
    updated: String,
    tags: String,
    requester_id: i64,
    requester_name: String,
    requester_initials: String,
    requester_email: String,
    requester_phone: String,
    org_id: i64,
    org_name: String,
    status_opts: Vec<Opt>,
    priority_opts: Vec<Opt>,
    type_opts: Vec<Opt>,
    assignee_opts: Vec<Opt>,
    group_opts: Vec<Opt>,
    orig_priority: String,
    orig_ticket_type: String,
    orig_assignee_id: String,
    orig_group_id: String,
    macros: Vec<Opt>,
    comments: Vec<CommentView>,
    audits: Vec<AuditView>,
    sla: Vec<SlaView>,
    custom_fields: Vec<KeyValue>,
    cf_inputs: Vec<CfInput>,
    cf_with_orig: bool,
    other_tickets: Vec<MiniTicket>,
    csat: String,
    csat_good: bool,
    csat_comment: String,
    kb_hits: Vec<KbHit>,
    calls: Vec<super::telephony::CallView>,
    links: Vec<LinkView>,
    link_kinds: Vec<Opt>,
    incident_count: usize,
    followup_source: i64,
    cc: String,
    time_total: String,
    time_mine: String,
    seen_audit: i64,
    draft: String,
    draft_internal: bool,
    warning: String,
    chat_id: i64,
    chat_status: String,
    chat_status_label: &'static str,
    chat_present: bool,
}

/// Nicht abgeschickter Text, der beim erneuten Anzeigen erhalten bleibt.
#[derive(Default)]
struct Draft {
    comment: String,
    internal: bool,
    warning: String,
}

/// Ein verknüpftes Ticket in der Seitenleiste.
struct LinkView {
    id: i64,
    subject: String,
    status: String,
    status_label: &'static str,
    relation: &'static str,
    removable: bool,
}

fn link_view(t: &Ticket, relation: &'static str, removable: bool) -> LinkView {
    LinkView {
        id: t.id,
        subject: t.subject.clone(),
        status: t.status.clone(),
        status_label: status_label(&t.status),
        relation,
        removable,
    }
}

fn mini(t: &Ticket) -> MiniTicket {
    MiniTicket { id: t.id, subject: t.subject.clone(), status: t.status.clone(), status_label: status_label(&t.status) }
}

/// Sekunden als „12 Min“ bzw. „1:05 h“.
pub fn fmt_seconds(s: i64) -> String {
    if s < 60 {
        return "< 1 Min".into();
    }
    let minutes = (s + 30) / 60;
    if minutes < 60 { format!("{minutes} Min") } else { format!("{}:{:02} h", minutes / 60, minutes % 60) }
}

/// Zeiteingabe „mm:ss“, „h:mm:ss“ oder Minuten („12“, „1,5“) in Sekunden.
fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split(':').collect();
    let secs = match parts.as_slice() {
        [m] => (m.replace(',', ".").parse::<f64>().ok()? * 60.0) as i64,
        [m, sec] => m.parse::<i64>().ok()? * 60 + sec.parse::<i64>().ok()?,
        [h, m, sec] => h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60 + sec.parse::<i64>().ok()?,
        _ => return None,
    };
    (secs > 0 && secs < 24 * 3600).then_some(secs)
}

struct KbHit {
    title: String,
    url: String,
    insert: String,
}

fn field_label(field: &str) -> String {
    match field {
        "subject" => "Betreff".into(),
        "status" => "Status".into(),
        "priority" => "Priorität".into(),
        "type" => "Typ".into(),
        "requester_id" => "Anfragender".into(),
        "assignee_id" => "Bearbeiter".into(),
        "group_id" => "Gruppe".into(),
        "organization_id" => "Organisation".into(),
        "tags" => "Tags".into(),
        "custom_fields" => "Eigene Felder".into(),
        "sla_policy_id" => "SLA".into(),
        "due_at" => "Fällig".into(),
        "problem_id" => "Problem".into(),
        "cc_ids" => "CC".into(),
        other => other.into(),
    }
}

/// Namen für IDs im Ereignisprotokoll.
struct Names {
    users: HashMap<i64, String>,
    groups: HashMap<i64, String>,
    orgs: HashMap<i64, String>,
    slas: HashMap<i64, String>,
    fields: HashMap<String, String>,
}

impl Names {
    fn label(&self, field: &str) -> String {
        match field.strip_prefix("custom_fields.") {
            Some(key) => self.fields.get(key).cloned().unwrap_or_else(|| key.to_string()),
            None => field_label(field),
        }
    }
}

impl Names {
    fn lookup(&self, field: &str, id: i64) -> Option<&String> {
        match field {
            "requester_id" | "assignee_id" => self.users.get(&id),
            "group_id" => self.groups.get(&id),
            "organization_id" => self.orgs.get(&id),
            "sla_policy_id" => self.slas.get(&id),
            _ => None,
        }
    }
}

fn describe_value(field: &str, v: &Value, names: &Names) -> String {
    match (field, v) {
        (_, Value::Null) => "–".into(),
        ("status", Value::String(s)) => status_label(s).into(),
        ("priority", Value::String(s)) => priority_label(s).into(),
        ("type", Value::String(s)) => type_label(s).into(),
        ("problem_id", Value::Number(n)) => format!("#{n}"),
        ("cc_ids", Value::Array(items)) if !items.is_empty() => items
            .iter()
            .filter_map(Value::as_i64)
            .map(|id| names.users.get(&id).cloned().unwrap_or_else(|| format!("#{id}")))
            .collect::<Vec<_>>()
            .join(", "),
        (f, Value::Number(n)) if f.ends_with("_id") => {
            let id = n.as_i64().unwrap_or_default();
            names.lookup(f, id).cloned().unwrap_or_else(|| format!("#{id} (gelöscht)"))
        }
        (_, Value::Array(items)) if items.is_empty() => "–".into(),
        (_, Value::Array(items)) => items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
        (_, Value::String(s)) => s.clone(),
        (_, Value::Bool(b)) => if *b { "Ja".into() } else { "Nein".into() },
        (_, other) => other.to_string(),
    }
}

fn describe_event(ev: &AuditEvent, names: &Names) -> String {
    match ev {
        AuditEvent::Create { field, value } => {
            format!("{}: {}", names.label(field), describe_value(field, value, names))
        }
        AuditEvent::Change { field, previous_value, value } => format!(
            "{}: {} → {}",
            names.label(field),
            describe_value(field, previous_value, names),
            describe_value(field, value, names)
        ),
        AuditEvent::Comment { public: true, .. } => "Öffentlicher Kommentar".into(),
        AuditEvent::Comment { public: false, .. } => "Interne Notiz".into(),
        AuditEvent::RuleApplied { kind, title, .. } if kind == "routing" => {
            format!("Automatisch zugewiesen ({title})")
        }
        AuditEvent::RuleApplied { kind, title, .. } if kind == "gdpr" => format!("Datenschutz: {title}"),
        AuditEvent::RuleApplied { kind, title, .. } => {
            let kind = match kind.as_str() {
                "macro" => "Makro",
                "automation" => "Automation",
                _ => "Trigger",
            };
            format!("{kind} „{title}“ ausgeführt")
        }
        AuditEvent::Notification { body } => format!("Benachrichtigung an Anfragenden: {body}"),
        AuditEvent::Link { ticket_id, added: true, .. } => format!("Mit Ticket #{ticket_id} verknüpft"),
        AuditEvent::Link { ticket_id, added: false, .. } => format!("Verknüpfung mit Ticket #{ticket_id} entfernt"),
    }
}

async fn show_ticket(State(st): State<AppState>, WebAgent(me): WebAgent, Path(id): Path<i64>) -> WebResult<Html<String>> {
    let html = render_ticket(&st, &me, id, Draft::default()).await?;
    crate::notify::mark_ticket_read(&st.db, me.id, id).await?;
    Ok(html)
}

async fn render_ticket(st: &AppState, me: &User, id: i64, draft: Draft) -> WebResult<Html<String>> {
    let db = &st.db;
    let t = service::get_ticket(db, id).await?;
    let now = Utc::now();
    let requester: User = sqlx::query_as("SELECT * FROM users WHERE id = $1").bind(t.requester_id).fetch_one(db).await?;
    let org: Option<Organization> = sqlx::query_as("SELECT * FROM organizations WHERE id = $1")
        .bind(t.organization_id)
        .fetch_optional(db)
        .await?;
    let comments: Vec<Comment> =
        sqlx::query_as("SELECT * FROM ticket_comments WHERE ticket_id = $1 ORDER BY created_at, id")
            .bind(id)
            .fetch_all(db)
            .await?;
    let audits: Vec<Audit> =
        sqlx::query_as("SELECT * FROM ticket_audits WHERE ticket_id = $1 ORDER BY created_at DESC, id DESC")
            .bind(id)
            .fetch_all(db)
            .await?;
    // Ein inzwischen deaktivierter Bearbeiter bleibt sichtbar, sonst würde das
    // Formular die Zuweisung beim nächsten Speichern stillschweigend entfernen.
    let agents: Vec<User> = sqlx::query_as(
        "SELECT * FROM users WHERE role IN ('agent', 'admin') AND (active OR id = $1) ORDER BY name",
    )
    .bind(t.assignee_id)
    .fetch_all(db)
    .await?;
    let agent_labels: Vec<(i64, String)> = agents
        .iter()
        .map(|a| (a.id, if a.active { a.name.clone() } else { format!("{} (deaktiviert)", a.name) }))
        .collect();
    let groups: Vec<Group> = sqlx::query_as("SELECT * FROM agent_groups ORDER BY name").fetch_all(db).await?;
    // Gemeinsame Makros zuerst, danach die persönlichen des Agenten.
    let macros: Vec<Macro> = sqlx::query_as(
        "SELECT * FROM macros WHERE active AND (owner_id IS NULL OR owner_id = $1)
         ORDER BY owner_id IS NOT NULL, position, id",
    )
    .bind(me.id)
    .fetch_all(db)
    .await?;
    // Alle Beziehungen zu anderen Tickets für die Karte „Verknüpfte Tickets“.
    let mut links: Vec<LinkView> = Vec::new();
    let one = |tid: Option<i64>| async move {
        match tid {
            Some(tid) => service::get_ticket(db, tid).await.ok(),
            None => None,
        }
    };
    if let Some(p) = one(t.problem_id).await {
        links.push(link_view(&p, "Ursache (Problem)", true));
    }
    let incidents: Vec<Ticket> = sqlx::query_as("SELECT * FROM tickets WHERE problem_id = $1 ORDER BY id")
        .bind(id)
        .fetch_all(db)
        .await?;
    links.extend(incidents.iter().map(|i| link_view(i, "Vorfall", true)));
    let related: Vec<Ticket> = sqlx::query_as(
        "SELECT t.* FROM ticket_links l JOIN tickets t ON t.id = CASE WHEN l.a_id = $1 THEN l.b_id ELSE l.a_id END
         WHERE l.a_id = $1 OR l.b_id = $1 ORDER BY t.id",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    links.extend(related.iter().map(|r| link_view(r, "verwandt", true)));
    if let Some(src) = one(t.followup_source_id).await {
        links.push(link_view(&src, "Ursprung", false));
    }
    let followups: Vec<Ticket> = sqlx::query_as("SELECT * FROM tickets WHERE followup_source_id = $1 ORDER BY id")
        .bind(id)
        .fetch_all(db)
        .await?;
    links.extend(followups.iter().map(|f| link_view(f, "Folgeticket", false)));
    let cc_emails: Vec<String> =
        sqlx::query_scalar("SELECT email FROM users WHERE id = ANY($1) AND email IS NOT NULL ORDER BY email")
            .bind(&t.cc_ids)
            .fetch_all(db)
            .await?;
    let (time_total, time_mine): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(sum(seconds), 0)::bigint, COALESCE(sum(seconds) FILTER (WHERE user_id = $2), 0)::bigint
         FROM time_entries WHERE ticket_id = $1",
    )
    .bind(id)
    .bind(me.id)
    .fetch_one(db)
    .await?;
    let seen_audit: i64 = sqlx::query_scalar("SELECT COALESCE(max(id), 0) FROM ticket_audits WHERE ticket_id = $1")
        .bind(id)
        .fetch_one(db)
        .await?;
    let chat = crate::chat::for_ticket(db, id).await?;
    let others: Vec<Ticket> = sqlx::query_as(
        "SELECT * FROM tickets WHERE requester_id = $1 AND id <> $2 ORDER BY updated_at DESC LIMIT 8",
    )
    .bind(t.requester_id)
    .bind(id)
    .fetch_all(db)
    .await?;
    let policy = sla::load_policy(db, t.sla_policy_id).await?;

    let attachments = super::files::attachments_by_comment(db, id).await?;
    let calls = super::telephony::call_views(db, &crate::telephony::for_ticket(db, id).await?).await?;
    let kb_hits: Vec<KbHit> = crate::kb::search(db, &t.subject, true, true, 3)
        .await?
        .into_iter()
        .map(|a| {
            let url = crate::kb::article_url(a.id);
            KbHit { insert: format!("{}: {}", a.title, url), title: a.title, url }
        })
        .collect();
    let mut ids: Vec<i64> = comments.iter().map(|c| c.author_id).collect();
    ids.extend(audits.iter().filter_map(|a| a.author_id));
    for a in &audits {
        for ev in &a.events.0 {
            if let AuditEvent::Change { field, previous_value, value } = ev {
                if field.ends_with("_id") {
                    ids.extend(previous_value.as_i64());
                    ids.extend(value.as_i64());
                }
            }
        }
    }
    ids.push(t.requester_id);
    let users = user_names(db, ids).await?;
    let author_roles: HashMap<i64, bool> = sqlx::query_as::<_, (i64, String)>("SELECT id, role FROM users WHERE id = ANY($1)")
        .bind(comments.iter().map(|c| c.author_id).collect::<Vec<_>>())
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|(id, role)| (id, role != "end-user"))
        .collect();
    let names = Names {
        users: users.clone(),
        groups: groups.iter().map(|g| (g.id, g.name.clone())).collect(),
        orgs: sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM organizations")
            .fetch_all(db)
            .await?
            .into_iter()
            .collect(),
        slas: sqlx::query_as::<_, (i64, String)>("SELECT id, title FROM sla_policies")
            .fetch_all(db)
            .await?
            .into_iter()
            .collect(),
        fields: sqlx::query_as::<_, (String, String)>("SELECT key, label FROM ticket_fields")
            .fetch_all(db)
            .await?
            .into_iter()
            .collect(),
    };

    let mut statuses: Vec<&str> = STATUSES.iter().copied().filter(|s| *s != "closed").collect();
    if t.status != "new" {
        statuses.retain(|s| *s != "new");
    }
    if t.status == "closed" {
        statuses = vec!["closed"];
    }

    let sla = policy
        .as_ref()
        .map(|p| sla::evaluate(p, &t, now))
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            let (text, class) = if s.fulfilled && !s.breached {
                ("erfüllt".to_string(), "sla ok")
            } else if s.fulfilled {
                ("verletzt (erledigt)".to_string(), "sla breached")
            } else if s.breached {
                (format!("überfällig {}", sla::fmt_minutes(-s.minutes_left)), "sla breached")
            } else {
                (format!("noch {}", sla::fmt_minutes(s.minutes_left)), if s.minutes_left < 60 { "sla warn" } else { "sla ok" })
            };
            SlaView { label: s.label, text, class }
        })
        .collect();

    let field_defs = service::ticket_fields(db, true).await?;
    let custom_fields = t
        .custom_fields
        .as_object()
        .map(|o| {
            o.iter()
                .filter(|(k, _)| !field_defs.iter().any(|f| &f.key == *k))
                .map(|(k, v)| KeyValue { key: k.clone(), value: v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()) })
                .collect()
        })
        .unwrap_or_default();

    page(TicketPage {
        nav: Nav::new(me, "views"),
        links,
        link_kinds: service::LINK_KINDS
            .iter()
            .map(|(v, l)| Opt { value: v.to_string(), label: l.to_string(), selected: *v == "related" })
            .collect(),
        incident_count: incidents.len(),
        followup_source: t.followup_source_id.unwrap_or(0),
        cc: cc_emails.join(", "),
        time_total: if time_total > 0 { fmt_seconds(time_total) } else { String::new() },
        time_mine: if time_mine > 0 { fmt_seconds(time_mine) } else { String::new() },
        seen_audit,
        draft: draft.comment,
        draft_internal: draft.internal,
        warning: draft.warning,
        chat_id: chat.as_ref().map(|c| c.id).unwrap_or(0),
        chat_status: chat.as_ref().map(|c| c.status.clone()).unwrap_or_default(),
        chat_status_label: chat.as_ref().map(|c| crate::chat::status_label(&c.status)).unwrap_or(""),
        chat_present: chat.as_ref().is_some_and(|c| crate::chat::visitor_present(c, now)),
        id: t.id,
        subject: t.subject.clone(),
        status: t.status.clone(),
        status_label: status_label(&t.status),
        closed: t.status == "closed",
        channel_label: channel_label(&t.channel),
        created: fmt_dt(t.created_at),
        updated: fmt_rel(t.updated_at, now),
        tags: t.tags.join(" "),
        requester_id: requester.id,
        requester_name: requester.name.clone(),
        requester_initials: initials(&requester.name),
        requester_email: requester.email.clone().unwrap_or_default(),
        requester_phone: requester.phone.clone().unwrap_or_default(),
        org_id: org.as_ref().map(|o| o.id).unwrap_or_default(),
        org_name: org.map(|o| o.name).unwrap_or_default(),
        status_opts: enum_opts(&statuses, status_label, Some(&t.status), None),
        priority_opts: enum_opts(&PRIORITIES, priority_label, t.priority.as_deref(), Some("–")),
        type_opts: enum_opts(&TYPES, type_label, t.ticket_type.as_deref(), Some("–")),
        assignee_opts: id_opts(agent_labels.iter().map(|(id, n)| (*id, n.as_str())), t.assignee_id, "– nicht zugewiesen –"),
        group_opts: id_opts(groups.iter().map(|g| (g.id, g.name.as_str())), t.group_id, "–"),
        orig_priority: t.priority.clone().unwrap_or_default(),
        orig_ticket_type: t.ticket_type.clone().unwrap_or_default(),
        orig_assignee_id: t.assignee_id.map(|id| id.to_string()).unwrap_or_default(),
        orig_group_id: t.group_id.map(|id| id.to_string()).unwrap_or_default(),
        macros: macros.iter().map(|m| Opt { value: m.id.to_string(), label: m.title.clone(), selected: false }).collect(),
        comments: comments
            .iter()
            .map(|c| {
                let author = users.get(&c.author_id).cloned().unwrap_or_else(|| format!("#{}", c.author_id));
                CommentView {
                    initials: initials(&author),
                    author,
                    is_agent: author_roles.get(&c.author_id).copied().unwrap_or(false),
                    public: c.public,
                    html: crate::markdown::render(&c.body),
                    created: fmt_dt(c.created_at),
                    via: channel_label(&c.via).replace('?', &c.via),
                    attachments: attachments.get(&c.id).map(|l| l.iter().map(Into::into).collect()).unwrap_or_default(),
                }
            })
            .collect(),
        audits: audits
            .iter()
            .map(|a| AuditView {
                created: fmt_dt(a.created_at),
                author: a
                    .author_id
                    .and_then(|id| users.get(&id).cloned())
                    .unwrap_or_else(|| "System".into()),
                lines: a.events.0.iter().map(|e| describe_event(e, &names)).collect(),
            })
            .collect(),
        sla,
        custom_fields,
        cf_inputs: cf_inputs(&field_defs, &t.custom_fields),
        cf_with_orig: true,
        csat: match t.satisfaction_score.as_deref() {
            Some("good") => "👍 Gut".into(),
            Some("bad") => "👎 Schlecht".into(),
            _ => String::new(),
        },
        csat_good: t.satisfaction_score.as_deref() == Some("good"),
        csat_comment: t.satisfaction_comment.clone().unwrap_or_default(),
        kb_hits,
        calls,
        other_tickets: others.iter().map(mini).collect(),
    })
}

/// Formularwerte plus die Werte beim Öffnen (`orig_*`), damit nur tatsächlich
/// geänderte Felder gespeichert werden und parallele Änderungen von Kollegen
/// nicht überschrieben werden.
#[derive(Deserialize)]
struct TicketForm {
    subject: Option<String>,
    orig_subject: Option<String>,
    status: Option<String>,
    orig_status: Option<String>,
    submit_as: Option<String>,
    priority: Option<String>,
    orig_priority: Option<String>,
    ticket_type: Option<String>,
    orig_ticket_type: Option<String>,
    assignee_id: Option<String>,
    orig_assignee_id: Option<String>,
    group_id: Option<String>,
    orig_group_id: Option<String>,
    tags: Option<String>,
    orig_tags: Option<String>,
    comment: Option<String>,
    public: Option<String>,
    cc: Option<String>,
    orig_cc: Option<String>,
    /// Erfasste Bearbeitungszeit („mm:ss“ oder Minuten)
    time_spent: Option<String>,
    /// Letzter bekannter Audit-Eintrag beim Öffnen (Kollisionserkennung)
    seen_audit: Option<i64>,
    /// Eigene Felder: cf_<key> und orig_cf_<key>
    #[serde(flatten)]
    extra: HashMap<String, String>,
}

/// Neuer Wert, falls er sich vom Wert beim Öffnen unterscheidet ("" = geleert).
fn changed(new: &Option<String>, orig: &Option<String>) -> Option<String> {
    let new = new.as_deref().map(str::trim).unwrap_or("");
    let orig = orig.as_deref().map(str::trim).unwrap_or("");
    (new != orig).then(|| new.to_string())
}

fn tag_set(s: &Option<String>) -> Vec<String> {
    normalize_tags(s.as_deref().unwrap_or("").split_whitespace())
}

/// E-Mail-Adressen (Komma, Semikolon oder Leerzeichen getrennt) in Personen-IDs; Unbekannte werden angelegt.
async fn cc_ids(db: &PgPool, text: &str) -> AppResult<Vec<i64>> {
    let mut ids = Vec::new();
    for addr in text.split([',', ';', ' ', '\n']).map(str::trim).filter(|a| !a.is_empty()) {
        if !addr.contains('@') {
            return Err(AppError::invalid(format!("CC: „{addr}“ ist keine E-Mail-Adresse")));
        }
        ids.push(service::find_or_create_requester(db, "", Some(addr), None).await?.id);
    }
    Ok(ids)
}

/// Öffentliche Antworten anderer seit dem Öffnen des Tickets (Namen der Autoren);
/// mit `agents_only` nur Antworten von Kollegen (Gefahr doppelter Antworten).
async fn replies_since(db: &PgPool, id: i64, seen: i64, me: i64, agents_only: bool) -> AppResult<Vec<String>> {
    // Kommentare einer Transaktion tragen denselben Zeitstempel wie ihr Audit-Eintrag.
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT u.name FROM ticket_comments c JOIN users u ON u.id = c.author_id
         WHERE c.ticket_id = $1 AND c.public AND c.author_id <> $3 AND (NOT $4 OR u.role <> 'end-user')
           AND c.created_at > COALESCE((SELECT created_at FROM ticket_audits WHERE id = $2), '-infinity')",
    )
    .bind(id)
    .bind(seen)
    .bind(me)
    .bind(agents_only)
    .fetch_all(db)
    .await?)
}

async fn update_ticket(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path(id): Path<i64>,
    Form(f): Form<TicketForm>,
) -> WebResult<Response> {
    let comment = non_empty(&f.comment);
    let public = f.public.as_deref() != Some("0");
    // Kollision: Hat jemand anderes inzwischen öffentlich geantwortet, wird die eigene
    // öffentliche Antwort nicht blind verschickt, sondern mit Hinweis wieder angezeigt.
    if let (Some(body), true, Some(seen)) = (&comment, public, f.seen_audit) {
        let others = replies_since(&st.db, id, seen, me.id, true).await?;
        if !others.is_empty() {
            let draft = Draft {
                comment: body.clone(),
                internal: false,
                warning: format!(
                    "{} hat inzwischen geantwortet (siehe unten). Deine Antwort wurde noch nicht gesendet – bitte prüfen und erneut absenden. Andere Feldänderungen bitte ebenfalls erneut setzen.",
                    others.join(", ")
                ),
            };
            let html = render_ticket(&st, &me, id, draft).await?;
            return Ok((StatusCode::CONFLICT, html).into_response());
        }
    }
    let fields = service::ticket_fields(&st.db, true).await?;
    let custom_fields = service::custom_field_changes(&fields, &f.extra, true)?;
    let (new_tags, orig_tags) = (tag_set(&f.tags), tag_set(&f.orig_tags));
    let added: Vec<String> = new_tags.iter().filter(|t| !orig_tags.contains(t)).cloned().collect();
    let removed: Vec<String> = orig_tags.iter().filter(|t| !new_tags.contains(t)).cloned().collect();
    let changes = TicketChanges {
        // Ein geleerter Betreff kommt als "" an und wird vom Service abgelehnt.
        subject: changed(&f.subject, &f.orig_subject),
        status: non_empty(&f.submit_as).or_else(|| changed(&f.status, &f.orig_status).filter(|s| !s.is_empty())),
        priority: changed(&f.priority, &f.orig_priority).map(|v| Some(v).filter(|v| !v.is_empty())),
        ticket_type: changed(&f.ticket_type, &f.orig_ticket_type).map(|v| Some(v).filter(|v| !v.is_empty())),
        assignee_id: changed(&f.assignee_id, &f.orig_assignee_id).map(|v| v.parse().ok()),
        group_id: changed(&f.group_id, &f.orig_group_id).map(|v| v.parse().ok()),
        custom_fields,
        additional_tags: (!added.is_empty()).then_some(added),
        remove_tags: (!removed.is_empty()).then_some(removed),

        cc_ids: match changed(&f.cc, &f.orig_cc) {
            Some(text) => Some(cc_ids(&st.db, &text).await?),
            None => None,
        },
        comment: comment.map(|body| CommentChange { body, public, author_id: None }),
        ..Default::default()
    };
    let time = match f.time_spent.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => Some(parse_duration(raw).ok_or_else(|| {
            AppError::invalid(format!("Zeit „{raw}“ nicht erkannt – bitte z. B. 12 (Minuten) oder 1:30:00 angeben"))
        })?),
        None => None,
    };
    service::update_ticket(&st.db, &me, id, changes, None, "web").await?;
    if let Some(seconds) = time {
        sqlx::query("INSERT INTO time_entries (ticket_id, user_id, seconds) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(me.id)
            .bind(seconds as i32)
            .execute(&st.db)
            .await?;
    }
    Ok(see_other(&format!("/tickets/{id}")))
}

#[derive(Deserialize)]
struct LinkForm {
    target: String,
    kind: Option<String>,
}

async fn add_link(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path(id): Path<i64>,
    Form(f): Form<LinkForm>,
) -> WebResult<Response> {
    let other: i64 = f
        .target
        .trim()
        .trim_start_matches('#')
        .parse()
        .map_err(|_| AppError::invalid("Bitte die Nummer des anderen Tickets angeben, z. B. 12"))?;
    service::link_tickets(&st.db, &me, id, other, f.kind.as_deref().unwrap_or("related")).await?;
    Ok(see_other(&format!("/tickets/{id}")))
}

async fn remove_link(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path((id, other)): Path<(i64, i64)>,
) -> WebResult<Response> {
    service::unlink_tickets(&st.db, &me, id, other).await?;
    Ok(see_other(&format!("/tickets/{id}")))
}

// ---------------------------------------------------------------------------
// Kollisionserkennung: wer ist gerade im Ticket, gibt es neue Änderungen?
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PresenceQuery {
    seen: Option<i64>,
    composing: Option<String>,
}

struct Viewer {
    name: String,
    composing: bool,
}

#[derive(Template)]
#[template(path = "ticket_presence.html")]
struct PresenceFragment {
    id: i64,
    viewers: Vec<Viewer>,
    updated_by: String,
    new_reply: bool,
    visitor_typing: String,
}

async fn presence(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path(id): Path<i64>,
    Query(q): Query<PresenceQuery>,
) -> WebResult<Html<String>> {
    let composing = matches!(q.composing.as_deref(), Some("1" | "true"));
    let others = st.presence.touch(id, me.id, composing);
    // Live-Chat: Tippen an den Besucher melden und umgekehrt anzeigen.
    let chat = crate::chat::for_ticket(&st.db, id).await?;
    if composing && chat.as_ref().is_some_and(|c| c.status != "ended") {
        crate::chat::agent_typing(&st.db, id).await?;
    }
    let visitor_typing = match chat.filter(|c| crate::chat::typing(c.visitor_typing_at, chrono::Utc::now())) {
        Some(c) => sqlx::query_scalar("SELECT name FROM users WHERE id = $1").bind(c.visitor_id).fetch_one(&st.db).await?,
        None => String::new(),
    };
    let names = user_names(&st.db, others.iter().map(|(uid, _)| *uid).collect()).await?;
    let mut viewers: Vec<Viewer> = others
        .into_iter()
        .filter_map(|(uid, composing)| names.get(&uid).map(|name| Viewer { name: name.clone(), composing }))
        .collect();
    viewers.sort_by(|a, b| a.name.cmp(&b.name));
    let (updated_by, new_reply) = match q.seen {
        Some(seen) => {
            let authors: Vec<String> = sqlx::query_scalar(
                "SELECT DISTINCT COALESCE(u.name, 'System') FROM ticket_audits a LEFT JOIN users u ON u.id = a.author_id
                 WHERE a.ticket_id = $1 AND a.id > $2 AND a.author_id IS DISTINCT FROM $3",
            )
            .bind(id)
            .bind(seen)
            .bind(me.id)
            .fetch_all(&st.db)
            .await?;
            let replies = replies_since(&st.db, id, seen, me.id, false).await?;
            (authors.join(", "), !replies.is_empty())
        }
        None => (String::new(), false),
    };
    page(PresenceFragment { id, viewers, updated_by, new_reply, visitor_typing })
}

#[derive(Deserialize)]
struct MergeForm {
    target: String,
}

async fn merge_ticket(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path(id): Path<i64>,
    Form(f): Form<MergeForm>,
) -> WebResult<Response> {
    let target: i64 = f
        .target
        .trim()
        .trim_start_matches('#')
        .parse()
        .map_err(|_| AppError::invalid("Bitte die Nummer des Zieltickets angeben, z. B. 12"))?;
    service::merge_tickets(&st.db, &me, id, target).await?;
    Ok(see_other(&format!("/tickets/{target}")))
}

#[derive(Deserialize)]
struct MacroForm {
    macro_id: i64,
}

async fn apply_macro(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path(id): Path<i64>,
    Form(f): Form<MacroForm>,
) -> WebResult<Response> {
    service::update_ticket(&st.db, &me, id, TicketChanges::default(), Some(f.macro_id), "web").await?;
    Ok(see_other(&format!("/tickets/{id}")))
}

// ---------------------------------------------------------------------------
// Neues Ticket / Anruf erfassen
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "ticket_new.html")]
struct NewTicketPage {
    nav: Nav,
    is_call: bool,
    requester_id: String,
    requester_label: String,
    channel_opts: Vec<Opt>,
    status_opts: Vec<Opt>,
    priority_opts: Vec<Opt>,
    type_opts: Vec<Opt>,
    assignee_opts: Vec<Opt>,
    group_opts: Vec<Opt>,
    cf_inputs: Vec<CfInput>,
    cf_with_orig: bool,
    call_id: String,
    phone_prefill: String,
    followup: String,
    subject_prefill: String,
}

#[derive(Deserialize)]
struct NewTicketQuery {
    channel: Option<String>,
    requester_id: Option<i64>,
    call_id: Option<i64>,
    followup: Option<i64>,
}

async fn new_ticket_page(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Query(q): Query<NewTicketQuery>,
) -> WebResult<Html<String>> {
    let channel = q.channel.filter(|c| CHANNELS.contains(&c.as_str())).unwrap_or_else(|| "phone".into());
    let is_call = channel == "phone";
    let agents: Vec<User> =
        sqlx::query_as("SELECT * FROM users WHERE role IN ('agent', 'admin') AND active ORDER BY name")
            .fetch_all(&st.db)
            .await?;
    let groups: Vec<Group> = sqlx::query_as("SELECT * FROM agent_groups ORDER BY name").fetch_all(&st.db).await?;
    // Aus dem Anruf-Banner: Anrufer übernehmen bzw. Nummer vorbelegen.
    let call = match q.call_id {
        Some(id) => Some(crate::telephony::get(&st.db, id).await?),
        None => None,
    };
    // Folgeticket zu einem geschlossenen Ticket: Anfragender und Betreff übernehmen.
    let source = match q.followup {
        Some(id) => Some(service::get_ticket(&st.db, id).await?),
        None => None,
    };
    let requester: Option<User> = sqlx::query_as("SELECT * FROM users WHERE id = $1")
        .bind(
            q.requester_id
                .or(call.as_ref().and_then(|c| c.customer_id))
                .or(source.as_ref().map(|t| t.requester_id)),
        )
        .fetch_optional(&st.db)
        .await?;
    page(NewTicketPage {
        nav: Nav::new(&me, "new"),
        is_call,
        requester_id: requester.as_ref().map(|u| u.id.to_string()).unwrap_or_default(),
        requester_label: requester.as_ref().map(requester_label).unwrap_or_default(),
        channel_opts: enum_opts(&CHANNELS, channel_label, Some(&channel), None),
        status_opts: enum_opts(&["new", "open", "pending", "solved"], status_label, Some(if is_call { "open" } else { "new" }), None),
        priority_opts: enum_opts(&PRIORITIES, priority_label, Some("normal"), Some("–")),
        type_opts: enum_opts(&TYPES, type_label, None, Some("–")),
        assignee_opts: id_opts(
            agents.iter().map(|a| (a.id, a.name.as_str())),
            if is_call { Some(me.id) } else { None },
            "– nicht zugewiesen –",
        ),
        group_opts: id_opts(groups.iter().map(|g| (g.id, g.name.as_str())), None, "–"),
        cf_inputs: cf_inputs(&service::ticket_fields(&st.db, true).await?, &json!({})),
        cf_with_orig: false,
        call_id: call.as_ref().map(|c| c.id.to_string()).unwrap_or_default(),
        phone_prefill: call.as_ref().map(|c| c.remote_number().to_string()).unwrap_or_default(),
        followup: source.as_ref().map(|t| t.id.to_string()).unwrap_or_default(),
        subject_prefill: source.as_ref().map(|t| format!("Folgeanfrage zu #{}: {}", t.id, t.subject)).unwrap_or_default(),
    })
}

fn requester_label(u: &User) -> String {
    let contact = u.email.clone().or_else(|| u.phone.clone()).unwrap_or_default();
    if contact.is_empty() { u.name.clone() } else { format!("{} <{}>", u.name, contact) }
}

#[derive(Deserialize)]
struct NewTicketForm {
    requester_id: Option<String>,
    requester_name: Option<String>,
    requester_email: Option<String>,
    requester_phone: Option<String>,
    subject: String,
    description: String,
    public: Option<String>,
    channel: String,
    status: Option<String>,
    priority: Option<String>,
    ticket_type: Option<String>,
    assignee_id: Option<String>,
    group_id: Option<String>,
    tags: Option<String>,
    call_id: Option<String>,
    followup_source_id: Option<String>,
    cc: Option<String>,
    #[serde(flatten)]
    extra: HashMap<String, String>,
}

async fn create_ticket(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Form(f): Form<NewTicketForm>,
) -> WebResult<Response> {
    let requester_id = match parse_id(&f.requester_id) {
        Some(id) => id,
        None => {
            service::find_or_create_requester(
                &st.db,
                f.requester_name.as_deref().unwrap_or(""),
                f.requester_email.as_deref(),
                f.requester_phone.as_deref(),
            )
            .await?
            .id
        }
    };
    let ticket = service::create_ticket(
        &st.db,
        &me,
        NewTicket {
            subject: f.subject,
            description: f.description,
            public: f.public.as_deref() != Some("0"),
            requester_id,
            status: non_empty(&f.status),
            priority: non_empty(&f.priority),
            ticket_type: non_empty(&f.ticket_type),
            channel: f.channel,
            assignee_id: parse_id(&f.assignee_id),
            group_id: parse_id(&f.group_id),
            tags: f.tags.as_deref().map(|t| normalize_tags(t.split_whitespace())).unwrap_or_default(),
            custom_fields: service::custom_field_changes(&service::ticket_fields(&st.db, true).await?, &f.extra, false)?
                .unwrap_or_else(|| json!({})),
            followup_source_id: parse_id(&f.followup_source_id),
            cc_ids: match non_empty(&f.cc) {
                Some(text) => cc_ids(&st.db, &text).await?,
                None => Vec::new(),
            },
            ..Default::default()
        },
        "web",
    )
    .await?;
    if let Some(call_id) = parse_id(&f.call_id) {
        crate::telephony::link_ticket(&st.db, call_id, ticket.id, &me).await?;
    }
    Ok(see_other(&format!("/tickets/{}", ticket.id)))
}

struct RequesterHit {
    id: i64,
    name: String,
    label: String,
    detail: String,
    open_tickets: i64,
}

#[derive(Template)]
#[template(path = "requester_results.html")]
struct RequesterResults {
    query: String,
    hits: Vec<RequesterHit>,
}

/// htmx-Fragment: Anrufer anhand von Telefonnummer, E-Mail oder Name finden.
async fn requester_lookup(
    State(st): State<AppState>,
    WebAgent(_): WebAgent,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Html<String>> {
    let query = ["requester_phone", "requester_email", "requester_name", "q"]
        .iter()
        .filter_map(|k| params.get(*k))
        .map(|v| v.trim())
        .find(|v| v.len() >= 2)
        .unwrap_or("")
        .to_string();
    let mut hits = Vec::new();
    if !query.is_empty() {
        let users = service::search_users(&st.db, &query, 6, true).await?;
        let counts: HashMap<i64, i64> = sqlx::query_as::<_, (i64, i64)>(
            "SELECT requester_id, count(*) FROM tickets
             WHERE requester_id = ANY($1) AND status IN ('new', 'open', 'pending', 'hold')
             GROUP BY requester_id",
        )
        .bind(users.iter().map(|u| u.id).collect::<Vec<_>>())
        .fetch_all(&st.db)
        .await?
        .into_iter()
        .collect();
        hits = users
            .iter()
            .filter(|u| !u.is_agent())
            .map(|u| RequesterHit {
                id: u.id,
                name: u.name.clone(),
                label: requester_label(u),
                detail: [u.email.clone(), u.phone.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · "),
                open_tickets: counts.get(&u.id).copied().unwrap_or(0),
            })
            .collect();
    }
    page(RequesterResults { query, hits })
}
