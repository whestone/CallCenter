//! Regel-Engine für Views, Triggers, Macros und SLA-Filter.
//!
//! Bedingungen folgen dem Zendesk-Schema `{"all": [...], "any": [...]}` mit
//! Einträgen `{"field", "operator", "value"}`; Aktionen sind `{"field", "value"}`.

use std::cmp::Ordering;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::models::{priority_rank, status_rank, Ticket, User, PRIORITIES, STATUSES, TYPES};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Conditions {
    #[serde(default)]
    pub all: Vec<Condition>,
    #[serde(default)]
    pub any: Vec<Condition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition {
    pub field: String,
    pub operator: String,
    #[serde(default)]
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub field: String,
    #[serde(default)]
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateType {
    Create,
    Change,
    /// Auswertung außerhalb eines Updates (Views, SLA-Zuordnung).
    None,
}

#[derive(Debug, Clone, Default)]
pub struct CommentInput {
    pub body: String,
    pub public: bool,
}

pub struct EvalCtx<'a> {
    pub before: Option<&'a Ticket>,
    pub update_type: UpdateType,
    pub comment: Option<&'a CommentInput>,
    pub actor: Option<&'a User>,
    pub now: DateTime<Utc>,
}

impl<'a> EvalCtx<'a> {
    pub fn passive(actor: Option<&'a User>, now: DateTime<Utc>) -> Self {
        Self { before: None, update_type: UpdateType::None, comment: None, actor, now }
    }
}

pub const CONDITION_FIELDS: &[&str] = &[
    "status",
    "priority",
    "type",
    "channel",
    "subject",
    "description",
    "assignee_id",
    "group_id",
    "requester_id",
    "organization_id",
    "tags",
    "sla_policy_id",
    "hours_since_created",
    "hours_since_updated",
    "hours_since_solved",
    "update_type",
    "comment_is_public",
    "comment_text",
    "current_user_role",
];

pub const OPERATORS: &[&str] = &[
    "is",
    "is_not",
    "less_than",
    "greater_than",
    "includes",
    "not_includes",
    "contains",
    "not_contains",
    "present",
    "not_present",
    "changed",
    "not_changed",
    "changed_to",
    "changed_from",
];

pub const ACTION_FIELDS: &[&str] = &[
    "status",
    "priority",
    "type",
    "assignee_id",
    "group_id",
    "subject",
    "set_tags",
    "add_tags",
    "remove_tags",
    "comment_value",
    "comment_private",
    "notify_requester",
];

impl Conditions {
    pub fn matches(&self, t: &Ticket, ctx: &EvalCtx) -> bool {
        self.all.iter().all(|c| c.matches(t, ctx))
            && (self.any.is_empty() || self.any.iter().any(|c| c.matches(t, ctx)))
    }

    pub fn validate(&self) -> Result<(), String> {
        self.all.iter().chain(self.any.iter()).try_for_each(Condition::validate)
    }
}

impl Condition {
    pub fn validate(&self) -> Result<(), String> {
        if !CONDITION_FIELDS.contains(&self.field.as_str()) && !self.field.starts_with("custom_fields.") {
            return Err(format!("Unbekanntes Bedingungsfeld: {}", self.field));
        }
        if !OPERATORS.contains(&self.operator.as_str()) {
            return Err(format!("Unbekannter Operator: {}", self.operator));
        }
        Ok(())
    }

    pub fn matches(&self, t: &Ticket, ctx: &EvalCtx) -> bool {
        let current = field_value(t, &self.field, ctx);
        let expected = resolve_value(&self.value, &self.field, ctx);
        match self.operator.as_str() {
            "is" => loose_eq(&current, &expected),
            "is_not" => !loose_eq(&current, &expected),
            "less_than" => compare(&self.field, &current, &expected) == Some(Ordering::Less),
            "greater_than" => compare(&self.field, &current, &expected) == Some(Ordering::Greater),
            "includes" => {
                let have = tag_list(&current);
                tag_list(&expected).iter().any(|x| have.contains(x))
            }
            "not_includes" => {
                let have = tag_list(&current);
                !tag_list(&expected).iter().any(|x| have.contains(x))
            }
            "contains" => text(&current).to_lowercase().contains(&text(&expected).to_lowercase()),
            "not_contains" => !text(&current).to_lowercase().contains(&text(&expected).to_lowercase()),
            "present" => !is_blank(&current),
            "not_present" => is_blank(&current),
            op @ ("changed" | "not_changed" | "changed_to" | "changed_from") => {
                let Some(before) = ctx.before else {
                    return op == "not_changed";
                };
                let previous = field_value(before, &self.field, ctx);
                let changed = previous != current;
                match op {
                    "changed" => changed,
                    "not_changed" => !changed,
                    "changed_to" => changed && loose_eq(&current, &expected),
                    _ => changed && loose_eq(&previous, &expected),
                }
            }
            _ => false,
        }
    }
}

pub fn field_value(t: &Ticket, field: &str, ctx: &EvalCtx) -> Value {
    match field {
        "status" => json!(t.status),
        "priority" => json!(t.priority),
        "type" => json!(t.ticket_type),
        "channel" => json!(t.channel),
        "subject" => json!(t.subject),
        "description" => json!(t.description),
        "assignee_id" => json!(t.assignee_id),
        "group_id" => json!(t.group_id),
        "requester_id" => json!(t.requester_id),
        "organization_id" => json!(t.organization_id),
        "sla_policy_id" => json!(t.sla_policy_id),
        "tags" => json!(t.tags),
        "hours_since_created" => json!((ctx.now - t.created_at).num_minutes() as f64 / 60.0),
        "hours_since_updated" => json!((ctx.now - t.updated_at).num_minutes() as f64 / 60.0),
        "hours_since_solved" => t
            .solved_at
            .map(|s| json!((ctx.now - s).num_minutes() as f64 / 60.0))
            .unwrap_or(Value::Null),
        "update_type" => match ctx.update_type {
            UpdateType::Create => json!("Create"),
            UpdateType::Change => json!("Change"),
            UpdateType::None => Value::Null,
        },
        "comment_is_public" => ctx.comment.map(|c| json!(c.public)).unwrap_or(Value::Null),
        "comment_text" => ctx.comment.map(|c| json!(c.body)).unwrap_or(Value::Null),
        "current_user_role" => ctx.actor.map(|u| json!(u.role)).unwrap_or(Value::Null),
        f => f
            .strip_prefix("custom_fields.")
            .and_then(|k| t.custom_fields.get(k).cloned())
            .unwrap_or(Value::Null),
    }
}

fn resolve_value(v: &Value, field: &str, ctx: &EvalCtx) -> Value {
    match v {
        Value::String(s) if s == "current_user" => {
            ctx.actor.map(|u| json!(u.id)).unwrap_or(Value::Null)
        }
        Value::String(s) if s.is_empty() && field.ends_with("_id") => Value::Null,
        _ => v.clone(),
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Bool(x), Value::String(s)) | (Value::String(s), Value::Bool(x)) => *s == x.to_string(),
        _ => match (num(a), num(b)) {
            (Some(x), Some(y)) => (x - y).abs() < 1e-9,
            _ => a == b,
        },
    }
}

fn compare(field: &str, a: &Value, b: &Value) -> Option<Ordering> {
    let rank = |v: &Value| -> Option<f64> {
        match field {
            "status" => v.as_str().and_then(status_rank).map(|r| r as f64),
            "priority" => v.as_str().and_then(priority_rank).map(|r| r as f64),
            _ => num(v),
        }
    };
    rank(a)?.partial_cmp(&rank(b)?)
}

fn is_blank(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Tags aus einem String ("a b c") oder Array lesen.
pub fn tag_list(v: &Value) -> Vec<String> {
    match v {
        Value::Array(items) => normalize_tags(items.iter().filter_map(Value::as_str)),
        Value::String(s) => normalize_tags(s.split_whitespace()),
        _ => Vec::new(),
    }
}

pub fn normalize_tags<I, S>(tags: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let tag = tag.as_ref().trim().to_lowercase().replace(' ', "_");
        if !tag.is_empty() && !out.contains(&tag) {
            out.push(tag);
        }
    }
    out
}

/// Nebenwirkungen von Aktionen, die erst beim Speichern ausgeführt werden.
#[derive(Debug, Default)]
pub struct Effects {
    pub comments: Vec<CommentInput>,
    pub notifications: Vec<String>,
}

impl Action {
    pub fn apply(&self, t: &mut Ticket, actor: Option<&User>, fx: &mut Effects) -> Result<(), String> {
        let v = &self.value;
        match self.field.as_str() {
            "status" => {
                let s = req_str(v)?;
                if !STATUSES.contains(&s.as_str()) || s == "new" {
                    return Err(format!("Ungültiger Status: {s}"));
                }
                t.status = s;
            }
            "priority" => t.priority = opt_enum(v, &PRIORITIES)?,
            "type" => t.ticket_type = opt_enum(v, &TYPES)?,
            "assignee_id" => t.assignee_id = opt_id(v, actor)?,
            "group_id" => t.group_id = opt_id(v, actor)?,
            "subject" => t.subject = req_str(v)?,
            "set_tags" => t.tags = tag_list(v),
            "add_tags" => {
                for tag in tag_list(v) {
                    if !t.tags.contains(&tag) {
                        t.tags.push(tag);
                    }
                }
            }
            "remove_tags" => {
                let remove = tag_list(v);
                t.tags.retain(|x| !remove.contains(x));
            }
            "comment_value" => fx.comments.push(CommentInput { body: req_str(v)?, public: true }),
            "comment_private" => fx.comments.push(CommentInput { body: req_str(v)?, public: false }),
            "notify_requester" => fx.notifications.push(req_str(v)?),
            f => match f.strip_prefix("custom_fields.") {
                Some(key) if !key.is_empty() => {
                    if !t.custom_fields.is_object() {
                        t.custom_fields = json!({});
                    }
                    t.custom_fields[key] = v.clone();
                }
                _ => return Err(format!("Unbekanntes Aktionsfeld: {f}")),
            },
        }
        Ok(())
    }
}

/// Prüft Aktionen durch Probeanwendung auf ein leeres Ticket.
pub fn validate_actions(actions: &[Action]) -> Result<(), String> {
    let mut t = Ticket { status: "open".into(), custom_fields: json!({}), ..Default::default() };
    let mut fx = Effects::default();
    actions.iter().try_for_each(|a| a.apply(&mut t, None, &mut fx))
}

fn req_str(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Ok(s.clone()),
        _ => Err("Wert muss ein nicht-leerer Text sein".into()),
    }
}

fn opt_enum(v: &Value, allowed: &[&str]) -> Result<Option<String>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.is_empty() => Ok(None),
        Value::String(s) if allowed.contains(&s.as_str()) => Ok(Some(s.clone())),
        other => Err(format!("Ungültiger Wert: {other}")),
    }
}

fn opt_id(v: &Value, actor: Option<&User>) -> Result<Option<i64>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.is_empty() => Ok(None),
        Value::String(s) if s == "current_user" => Ok(actor.map(|u| u.id)),
        Value::String(s) => s.parse().map(Some).map_err(|_| format!("Ungültige ID: {s}")),
        Value::Number(n) => n.as_i64().map(Some).ok_or_else(|| format!("Ungültige ID: {n}")),
        other => Err(format!("Ungültige ID: {other}")),
    }
}

/// Einfache Platzhalter à la Zendesk-Liquid: `{{ticket.id}}`, `{{current_user.name}}` …
pub fn render_placeholders(text: &str, t: &Ticket, actor: Option<&User>) -> String {
    text.replace("{{ticket.id}}", &t.id.to_string())
        .replace("{{ticket.subject}}", &t.subject)
        .replace("{{ticket.status}}", crate::models::status_label(&t.status))
        .replace("{{ticket.priority}}", t.priority.as_deref().map(crate::models::priority_label).unwrap_or("–"))
        .replace("{{current_user.name}}", actor.map(|u| u.name.as_str()).unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket() -> Ticket {
        Ticket {
            id: 1,
            subject: "Drucker defekt".into(),
            status: "open".into(),
            priority: Some("high".into()),
            channel: "phone".into(),
            assignee_id: Some(7),
            tags: vec!["vip".into(), "anruf".into()],
            custom_fields: json!({}),
            ..Default::default()
        }
    }

    fn cond(field: &str, operator: &str, value: Value) -> Condition {
        Condition { field: field.into(), operator: operator.into(), value }
    }

    #[test]
    fn status_ordering() {
        let t = ticket();
        let ctx = EvalCtx::passive(None, Utc::now());
        assert!(cond("status", "less_than", json!("solved")).matches(&t, &ctx));
        assert!(!cond("status", "greater_than", json!("pending")).matches(&t, &ctx));
        assert!(cond("priority", "greater_than", json!("normal")).matches(&t, &ctx));
    }

    #[test]
    fn current_user_and_tags() {
        let t = ticket();
        let me = User {
            id: 7,
            name: "A".into(),
            email: None,
            phone: None,
            role: "agent".into(),
            organization_id: None,
            password_hash: None,
            active: true,
            availability: "online".into(),
            capacity: 10,
            totp_secret: None,
            totp_pending_secret: None,
            extension: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let ctx = EvalCtx::passive(Some(&me), Utc::now());
        assert!(cond("assignee_id", "is", json!("current_user")).matches(&t, &ctx));
        assert!(cond("tags", "includes", json!("vip other")).matches(&t, &ctx));
        assert!(cond("tags", "not_includes", json!("spam")).matches(&t, &ctx));
        assert!(cond("subject", "contains", json!("drucker")).matches(&t, &ctx));
    }

    #[test]
    fn changed_operators() {
        let before = ticket();
        let mut after = ticket();
        after.status = "solved".into();
        let ctx = EvalCtx {
            before: Some(&before),
            update_type: UpdateType::Change,
            comment: None,
            actor: None,
            now: Utc::now(),
        };
        assert!(cond("status", "changed_to", json!("solved")).matches(&after, &ctx));
        assert!(cond("status", "changed_from", json!("open")).matches(&after, &ctx));
        assert!(cond("priority", "not_changed", Value::Null).matches(&after, &ctx));
    }

    #[test]
    fn actions_apply() {
        let mut t = ticket();
        let mut fx = Effects::default();
        let actions = [
            Action { field: "status".into(), value: json!("pending") },
            Action { field: "add_tags".into(), value: json!("Rückruf") },
            Action { field: "remove_tags".into(), value: json!(["vip"]) },
            Action { field: "comment_private".into(), value: json!("Notiz") },
        ];
        for a in &actions {
            a.apply(&mut t, None, &mut fx).unwrap();
        }
        assert_eq!(t.status, "pending");
        assert_eq!(t.tags, vec!["anruf", "rückruf"]);
        assert_eq!(fx.comments.len(), 1);
        assert!(validate_actions(&[Action { field: "bogus".into(), value: json!(1) }]).is_err());
    }
}
