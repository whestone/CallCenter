use std::collections::HashMap;

use askama::Template;
use axum::{
    response::{Html, Response},
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
    body: String,
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
    }
}

async fn show_ticket(State(st): State<AppState>, WebAgent(me): WebAgent, Path(id): Path<i64>) -> WebResult<Html<String>> {
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
    let macros: Vec<Macro> = sqlx::query_as("SELECT * FROM macros WHERE active ORDER BY position, id")
        .fetch_all(db)
        .await?;
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
        nav: Nav::new(&me, "views"),
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
                    body: c.body.clone(),
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
        other_tickets: others
            .iter()
            .map(|o| MiniTicket {
                id: o.id,
                subject: o.subject.clone(),
                status: o.status.clone(),
                status_label: status_label(&o.status),
            })
            .collect(),
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

async fn update_ticket(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path(id): Path<i64>,
    Form(f): Form<TicketForm>,
) -> WebResult<Response> {
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
        comment: non_empty(&f.comment).map(|body| CommentChange {
            body,
            public: f.public.as_deref() != Some("0"),
            author_id: None,
        }),
        ..Default::default()
    };
    service::update_ticket(&st.db, &me, id, changes, None, "web").await?;
    Ok(see_other(&format!("/tickets/{id}")))
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
}

#[derive(Deserialize)]
struct NewTicketQuery {
    channel: Option<String>,
    requester_id: Option<i64>,
    call_id: Option<i64>,
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
    let requester: Option<User> = sqlx::query_as("SELECT * FROM users WHERE id = $1")
        .bind(q.requester_id.or(call.as_ref().and_then(|c| c.customer_id)))
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
            due_at: None,
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
