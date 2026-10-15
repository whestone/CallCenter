//! Persönliche Einstellungen der Ticketlisten: welche Filter und Felder ein Agent sieht.

use std::collections::HashSet;

use axum::{response::Response, routing::post, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::extract::{Form, State};
use super::*;
use crate::auth::WebAgent;

pub fn router() -> Router<AppState> {
    Router::new().route("/me/list-prefs", post(save))
}

/// Filter der Ticketlisten (Schlüssel = Parameter der Filterleiste).
pub const FILTERS: [(&str, &str); 6] = [
    ("q", "Suche"),
    ("status", "Status"),
    ("priority", "Priorität"),
    ("assignee", "Bearbeiter"),
    ("group", "Gruppe"),
    ("channel", "Kanal"),
];

/// Felder: (Schlüssel, Bezeichnung, standardmäßig sichtbar, Spalte statt Angabe unter dem Betreff).
pub const FIELDS: [(&str, &str, bool, bool); 10] = [
    ("requester", "Kunde & Firma", true, false),
    ("channel", "Kanal", true, false),
    ("group", "Gruppe", true, false),
    ("tags", "Tags", true, false),
    ("priority", "Priorität", true, true),
    ("type", "Typ", false, true),
    ("assignee", "Bearbeiter", true, true),
    ("sla", "SLA", true, true),
    ("created", "Erstellt", false, true),
    ("updated", "Aktualisiert", true, true),
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    hidden_filters: Vec<String>,
    /// Sichtbare Felder; `None` = Standard.
    #[serde(default)]
    fields: Option<Vec<String>>,
    /// Reihenfolge aller Felder; `None` = Standard.
    #[serde(default)]
    order: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct ListPrefs {
    hidden_filters: HashSet<String>,
    fields: HashSet<String>,
    /// Alle Feld-Schlüssel in der gewünschten Reihenfolge.
    order: Vec<&'static str>,
}

impl Default for ListPrefs {
    fn default() -> Self {
        ListPrefs {
            hidden_filters: HashSet::new(),
            fields: FIELDS.iter().filter(|f| f.2).map(|f| f.0.to_string()).collect(),
            order: FIELDS.iter().map(|f| f.0).collect(),
        }
    }
}

fn field(key: &str) -> Option<&'static (&'static str, &'static str, bool, bool)> {
    FIELDS.iter().find(|f| f.0 == key)
}

pub struct Choice {
    pub key: &'static str,
    pub label: &'static str,
    pub on: bool,
}

impl ListPrefs {
    /// Ist dieser Filter sichtbar?
    pub fn filter(&self, key: &str) -> bool {
        !self.hidden_filters.contains(key)
    }

    /// Ist dieses Feld sichtbar?
    pub fn show(&self, key: &str) -> bool {
        self.fields.contains(key)
    }

    fn ordered(&self, column: bool) -> impl Iterator<Item = &'static (&'static str, &'static str, bool, bool)> + '_ {
        self.order.iter().filter_map(|k| field(k)).filter(move |f| f.3 == column)
    }

    /// Sichtbare Spalten in Anzeigereihenfolge.
    pub fn columns(&self) -> Vec<(&'static str, &'static str)> {
        self.ordered(true).filter(|f| self.show(f.0)).map(|f| (f.0, f.1)).collect()
    }

    /// Sichtbare Angaben unter dem Betreff in Anzeigereihenfolge.
    pub fn meta(&self) -> Vec<&'static str> {
        self.ordered(false).filter(|f| self.show(f.0)).map(|f| f.0).collect()
    }

    pub fn filter_choices(&self) -> Vec<Choice> {
        FILTERS.iter().map(|(key, label)| Choice { key, label, on: self.filter(key) }).collect()
    }

    pub fn meta_choices(&self) -> Vec<Choice> {
        self.ordered(false).map(|f| Choice { key: f.0, label: f.1, on: self.show(f.0) }).collect()
    }

    pub fn column_choices(&self) -> Vec<Choice> {
        self.ordered(true).map(|f| Choice { key: f.0, label: f.1, on: self.show(f.0) }).collect()
    }
}

pub async fn load(db: &PgPool, user_id: i64) -> AppResult<ListPrefs> {
    let stored: Option<sqlx::types::Json<Stored>> =
        sqlx::query_scalar("SELECT ticket_list FROM user_prefs WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(db)
            .await?;
    let Some(sqlx::types::Json(s)) = stored else { return Ok(ListPrefs::default()) };
    let mut prefs = ListPrefs::default();
    prefs.hidden_filters = s.hidden_filters.into_iter().collect();
    if let Some(fields) = s.fields {
        prefs.fields = fields.into_iter().filter(|f| FIELDS.iter().any(|d| d.0 == f)).collect();
    }
    if let Some(order) = s.order {
        // Gespeicherte Reihenfolge, neu hinzugekommene Felder hinten anhängen.
        let mut keys: Vec<&'static str> = order.iter().filter_map(|k| field(k)).map(|f| f.0).collect();
        keys.dedup();
        for f in FIELDS.iter() {
            if !keys.contains(&f.0) {
                keys.push(f.0);
            }
        }
        prefs.order = keys;
    }
    Ok(prefs)
}

async fn save(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Form(pairs): Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    let back = pairs
        .iter()
        .find(|(k, _)| k == "return_to")
        .map(|(_, v)| v.as_str())
        .filter(|v| v.starts_with('/') && !v.starts_with("//"))
        .unwrap_or("/views")
        .to_string();
    if pairs.iter().any(|(k, _)| k == "reset") {
        sqlx::query("DELETE FROM user_prefs WHERE user_id = $1").bind(me.id).execute(&st.db).await?;
        return Ok(see_other_ok(&back, "Standardansicht wiederhergestellt"));
    }
    let checked = |name: &str| -> HashSet<&str> { pairs.iter().filter(|(k, _)| k == name).map(|(_, v)| v.as_str()).collect() };
    let filters = checked("filter");
    let fields = checked("field");
    // Reihenfolge = Reihenfolge der Felder im Formular (per Ziehen sortiert).
    let order: Vec<&str> = pairs.iter().filter(|(k, v)| k == "order" && field(v).is_some()).map(|(_, v)| v.as_str()).collect();
    let stored = json!({
        "hidden_filters": FILTERS.iter().map(|f| f.0).filter(|k| !filters.contains(k)).collect::<Vec<_>>(),
        "fields": FIELDS.iter().map(|f| f.0).filter(|k| fields.contains(k)).collect::<Vec<_>>(),
        "order": order,
    });
    sqlx::query(
        "INSERT INTO user_prefs (user_id, ticket_list, updated_at) VALUES ($1, $2, now())
         ON CONFLICT (user_id) DO UPDATE SET ticket_list = $2, updated_at = now()",
    )
    .bind(me.id)
    .bind(stored)
    .execute(&st.db)
    .await?;
    Ok(see_other_ok(&back, "Ansicht angepasst"))
}
