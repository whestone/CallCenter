use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{types::Json, FromRow};

use crate::rules::{Action, Conditions};
use crate::sla::SlaMetric;

pub const STATUSES: [&str; 6] = ["new", "open", "pending", "hold", "solved", "closed"];
pub const PRIORITIES: [&str; 4] = ["low", "normal", "high", "urgent"];
pub const TYPES: [&str; 4] = ["question", "incident", "problem", "task"];
pub const CHANNELS: [&str; 5] = ["web", "email", "phone", "api", "chat"];
pub const ROLES: [&str; 3] = ["end-user", "agent", "admin"];

pub fn status_rank(s: &str) -> Option<usize> {
    STATUSES.iter().position(|x| *x == s)
}

pub fn priority_rank(s: &str) -> Option<usize> {
    PRIORITIES.iter().position(|x| *x == s)
}

pub fn status_label(s: &str) -> &'static str {
    match s {
        "new" => "Neu",
        "open" => "Offen",
        "pending" => "Wartend",
        "hold" => "Angehalten",
        "solved" => "Gelöst",
        "closed" => "Geschlossen",
        _ => "?",
    }
}

pub fn priority_label(s: &str) -> &'static str {
    match s {
        "low" => "Niedrig",
        "normal" => "Normal",
        "high" => "Hoch",
        "urgent" => "Dringend",
        _ => "–",
    }
}

pub fn type_label(s: &str) -> &'static str {
    match s {
        "question" => "Frage",
        "incident" => "Vorfall",
        "problem" => "Problem",
        "task" => "Aufgabe",
        _ => "–",
    }
}

pub fn channel_label(s: &str) -> &'static str {
    match s {
        "web" => "Web",
        "email" => "E-Mail",
        "phone" => "Telefon",
        "api" => "API",
        "chat" => "Chat",
        _ => "?",
    }
}

pub const AVAILABILITIES: [&str; 3] = ["online", "away", "offline"];
pub const ASSIGNMENTS: [&str; 3] = ["manual", "round_robin", "least_loaded"];

pub fn availability_label(s: &str) -> &'static str {
    match s {
        "online" => "Online",
        "away" => "Abwesend",
        "offline" => "Offline",
        _ => "?",
    }
}

pub fn assignment_label(s: &str) -> &'static str {
    match s {
        "manual" => "Manuell",
        "round_robin" => "Reihum (Round Robin)",
        "least_loaded" => "Geringste Auslastung",
        _ => "?",
    }
}

pub fn role_label(s: &str) -> &'static str {
    match s {
        "end-user" => "Endkunde",
        "agent" => "Agent",
        "admin" => "Administrator",
        _ => "?",
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: String,
    pub organization_id: Option<i64>,
    #[serde(skip)]
    pub password_hash: Option<String>,
    pub active: bool,
    pub availability: String,
    pub capacity: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip)]
    pub totp_secret: Option<String>,
    #[serde(skip)]
    pub totp_pending_secret: Option<String>,
    /// Kennung beim Telefonie-Anbieter (Nebenstelle, sipgate-Benutzer-ID …).
    pub extension: Option<String>,
}

impl User {
    pub fn is_agent(&self) -> bool {
        self.role == "agent" || self.role == "admin"
    }

    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Organization {
    pub id: i64,
    pub name: String,
    pub domain_names: Vec<String>,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Group {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub assignment: String,
    pub last_assigned_user_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, FromRow)]
pub struct Ticket {
    pub id: i64,
    pub subject: String,
    pub description: String,
    pub status: String,
    pub priority: Option<String>,
    #[serde(rename = "type")]
    pub ticket_type: Option<String>,
    pub channel: String,
    pub requester_id: i64,
    pub submitter_id: i64,
    pub assignee_id: Option<i64>,
    pub group_id: Option<i64>,
    pub organization_id: Option<i64>,
    pub tags: Vec<String>,
    pub custom_fields: Value,
    pub sla_policy_id: Option<i64>,
    pub due_at: Option<DateTime<Utc>>,
    pub first_reply_at: Option<DateTime<Utc>>,
    pub solved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub satisfaction_score: Option<String>,
    pub satisfaction_comment: Option<String>,
    pub satisfaction_at: Option<DateTime<Utc>>,
    /// Vorfall → zugehöriges Problem-Ticket.
    pub problem_id: Option<i64>,
    /// Folgeticket zu diesem (geschlossenen) Ticket.
    pub followup_source_id: Option<i64>,
    /// Weitere Kunden in CC.
    pub cc_ids: Vec<i64>,
    #[serde(skip)]
    pub sla_warn_key: Option<String>,
    #[serde(skip)]
    pub sla_breach_key: Option<String>,
}

pub const FIELD_TYPES: [&str; 5] = ["text", "number", "dropdown", "checkbox", "date"];

pub fn field_type_label(s: &str) -> &'static str {
    match s {
        "text" => "Text",
        "number" => "Zahl",
        "dropdown" => "Auswahlliste",
        "checkbox" => "Ja/Nein",
        "date" => "Datum",
        _ => "?",
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct TicketField {
    pub id: i64,
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub options: Vec<String>,
    pub required_on_solve: bool,
    pub position: i32,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

impl TicketField {
    /// Formularwert in den gespeicherten JSON-Wert umwandeln ("" = leer).
    pub fn coerce(&self, raw: &str) -> Result<Value, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(Value::Null);
        }
        let invalid = || format!("Ungültiger Wert für „{}“: {raw}", self.label);
        Ok(match self.field_type.as_str() {
            "number" => {
                let n: f64 = raw.replace(',', ".").parse().map_err(|_| invalid())?;
                serde_json::Number::from_f64(n).map(Value::Number).ok_or_else(invalid)?
            }
            "checkbox" => match raw {
                "true" | "ja" | "on" | "1" => Value::Bool(true),
                "false" | "nein" | "0" => Value::Bool(false),
                _ => return Err(invalid()),
            },
            "dropdown" if !self.options.iter().any(|o| o == raw) => return Err(invalid()),
            "date" if chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").is_err() => return Err(invalid()),
            _ => Value::String(raw.to_string()),
        })
    }

    /// Gespeicherten Wert als Formularwert darstellen.
    pub fn display(&self, v: Option<&Value>) -> String {
        match v {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(Value::Bool(b)) => b.to_string(),
            Some(Value::Number(n)) => n.to_string(),
            Some(other) => other.to_string(),
        }
    }
}

/// Anhang ohne Dateninhalt (für Listen).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AttachmentInfo {
    pub id: i64,
    pub ticket_id: i64,
    pub comment_id: Option<i64>,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
    pub created_at: DateTime<Utc>,
}

pub fn fmt_size(bytes: i64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{} KB", b / 1024),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)).replace('.', ","),
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Comment {
    pub id: i64,
    pub ticket_id: i64,
    pub author_id: i64,
    pub body: String,
    pub public: bool,
    pub via: String,
    pub created_at: DateTime<Utc>,
}

/// Einzelnes Ereignis eines Ticket-Audits (wie `audit.events` bei Zendesk).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AuditEvent {
    Create { field: String, value: Value },
    Change { field: String, previous_value: Value, value: Value },
    Comment { comment_id: i64, public: bool },
    RuleApplied { kind: String, id: i64, title: String },
    Notification { body: String },
    /// Verknüpfung mit einem anderen Ticket angelegt (`added`) oder entfernt.
    Link { ticket_id: i64, relation: String, added: bool },
    /// Kommentar oder Anhang gelöscht (Inhalt wird nicht aufbewahrt).
    Deleted { item: String, name: String },
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Audit {
    pub id: i64,
    pub ticket_id: i64,
    pub author_id: Option<i64>,
    pub via: String,
    pub events: Json<Vec<AuditEvent>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewExecution {
    #[serde(default = "default_sort_by")]
    pub sort_by: String,
    #[serde(default = "default_sort_order")]
    pub sort_order: String,
}

fn default_sort_by() -> String {
    "updated_at".into()
}

fn default_sort_order() -> String {
    "desc".into()
}

impl Default for ViewExecution {
    fn default() -> Self {
        Self { sort_by: default_sort_by(), sort_order: default_sort_order() }
    }
}

pub const VIEW_SORT_FIELDS: [&str; 6] = ["created_at", "updated_at", "priority", "status", "id", "due_at"];

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct View {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub position: i32,
    pub active: bool,
    pub conditions: Json<Conditions>,
    pub execution: Json<ViewExecution>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Macro {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub position: i32,
    pub active: bool,
    pub actions: Json<Vec<Action>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Persönliches Makro eines Agenten (sonst für alle).
    pub owner_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Trigger {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub position: i32,
    pub active: bool,
    pub conditions: Json<Conditions>,
    pub actions: Json<Vec<Action>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SlaPolicy {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub position: i32,
    pub active: bool,
    pub filter: Json<Conditions>,
    pub metrics: Json<Vec<SlaMetric>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Geschäftszeiten für Fälligkeiten; wird beim Laden für die Anzeige gesetzt.
    #[sqlx(skip)]
    #[serde(skip)]
    pub schedule: Option<std::sync::Arc<crate::sla::Schedule>>,
}
