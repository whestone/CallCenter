//! Persönliche Einstellungen der Listen (Tickets, Kunden, Organisationen, Chats, Team):
//! welche Filter und Felder ein Agent sieht, in welcher Reihenfolge und ob ein Feld
//! als Spalte oder unter der Hauptangabe (Betreff bzw. Name) steht.

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

/// Feld: (Schlüssel, Bezeichnung, standardmäßig sichtbar, standardmäßig als Spalte statt darunter).
/// Jedes Feld lässt sich in beiden Bereichen anzeigen; der Nutzer kann es verschieben.
pub type Field = (&'static str, &'static str, bool, bool);

/// Beschreibung einer anpassbaren Liste.
pub struct Spec {
    /// Schlüssel der Liste (Formularwert `list`).
    pub key: &'static str,
    /// Bezeichnung der Hauptangabe, unter der die übrigen Felder stehen können.
    pub main: &'static str,
    /// Filter (Schlüssel = Parameter der Filterleiste).
    pub filters: &'static [(&'static str, &'static str)],
    pub fields: &'static [Field],
}

pub static TICKETS: Spec = Spec {
    key: "tickets",
    main: "Betreff",
    filters: &[
        ("q", "Suche"),
        ("status", "Status"),
        ("priority", "Priorität"),
        ("assignee", "Bearbeiter"),
        ("group", "Gruppe"),
        ("channel", "Kanal"),
    ],
    fields: &[
        ("status", "Status", true, false),
        ("id", "Ticketnummer", true, false),
        ("preview", "Textvorschau", true, false),
        ("requester", "Kunde", true, false),
        ("org", "Organisation", true, false),
        ("channel", "Kanal", true, false),
        ("group", "Gruppe", true, false),
        ("tags", "Tags", true, false),
        ("priority", "Priorität", true, true),
        ("type", "Typ", false, true),
        ("assignee", "Bearbeiter", true, true),
        ("sla", "SLA", true, true),
        ("last_reply", "Letzte Antwort an Kunde", false, true),
        ("last_customer", "Letzte Kundennachricht", false, true),
        ("due", "Fällig", false, true),
        ("solved", "Gelöst", false, true),
        ("satisfaction", "Bewertung", false, true),
        ("created", "Erstellt", false, true),
        ("updated", "Zuletzt aktualisiert", true, true),
    ],
};

pub static USERS: Spec = Spec {
    key: "users",
    main: "Namen",
    filters: &[("q", "Suche"), ("org", "Organisation")],
    fields: &[
        ("email", "E-Mail", true, false),
        ("phone", "Telefon", true, false),
        ("org", "Organisation", true, true),
        ("open", "Offene Tickets", true, true),
        ("created", "Erstellt", false, true),
        ("updated", "Zuletzt aktualisiert", true, true),
    ],
};

pub static ORGS: Spec = Spec {
    key: "orgs",
    main: "Namen",
    filters: &[("q", "Suche")],
    fields: &[
        ("domains", "Domains", true, false),
        ("notes", "Notizen", false, false),
        ("users", "Kunden", true, true),
        ("open", "Offene Tickets", true, true),
        ("created", "Erstellt", false, true),
        ("updated", "Zuletzt aktualisiert", false, true),
    ],
};

pub static CHATS: Spec = Spec {
    key: "chats",
    main: "Anliegen",
    filters: &[("status", "Status")],
    fields: &[
        ("visitor", "Besucher", true, false),
        ("email", "E-Mail", true, false),
        ("last", "Letzte Nachricht", true, false),
        ("page", "Seite", false, false),
        ("status", "Status", false, true),
        ("agent", "Agent", true, true),
        ("since", "Seit", true, true),
        ("updated", "Zuletzt aktualisiert", false, true),
    ],
};

pub static AGENTS: Spec = Spec {
    key: "agents",
    main: "Namen",
    filters: &[("q", "Suche"), ("role", "Rolle"), ("availability", "Erreichbarkeit")],
    fields: &[
        ("email", "E-Mail", true, false),
        ("role", "Rolle", true, true),
        ("groups", "Gruppen", true, true),
        ("skills", "Fähigkeiten", false, true),
        ("availability", "Erreichbarkeit", true, true),
        ("load", "Offen / Kapazität", true, true),
        ("active", "Konto", true, true),
        ("updated", "Zuletzt aktualisiert", false, true),
    ],
};

static SPECS: [&Spec; 5] = [&TICKETS, &USERS, &ORGS, &CHATS, &AGENTS];

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
    /// Felder, die als Spalte statt unter der Hauptangabe stehen; `None` = Standard.
    #[serde(default)]
    columns: Option<Vec<String>>,
}

#[derive(Clone)]
pub struct ListPrefs {
    pub spec: &'static Spec,
    hidden_filters: HashSet<String>,
    fields: HashSet<String>,
    /// Alle Feld-Schlüssel in der gewünschten Reihenfolge.
    order: Vec<&'static str>,
    /// Felder, die als eigene Spalte erscheinen (die übrigen unter der Hauptangabe).
    as_column: HashSet<&'static str>,
}

pub struct Choice {
    pub key: &'static str,
    pub label: &'static str,
    pub on: bool,
}

impl ListPrefs {
    pub fn defaults(spec: &'static Spec) -> Self {
        ListPrefs {
            spec,
            hidden_filters: HashSet::new(),
            fields: spec.fields.iter().filter(|f| f.2).map(|f| f.0.to_string()).collect(),
            order: spec.fields.iter().map(|f| f.0).collect(),
            as_column: spec.fields.iter().filter(|f| f.3).map(|f| f.0).collect(),
        }
    }

    fn field(&self, key: &str) -> Option<&'static Field> {
        self.spec.fields.iter().find(|f| f.0 == key)
    }

    fn apply(mut self, s: Stored) -> Self {
        self.hidden_filters = s.hidden_filters.into_iter().collect();
        // Felder, die es beim Speichern noch nicht gab, bekommen ihre Standardwerte.
        let known: Option<HashSet<&str>> = s.order.as_ref().map(|o| o.iter().map(String::as_str).collect());
        let is_new = |key: &str| known.as_ref().is_some_and(|k| !k.contains(key));
        if let Some(fields) = &s.fields {
            let mut set: HashSet<String> = fields.iter().filter(|f| self.field(f).is_some()).cloned().collect();
            set.extend(self.spec.fields.iter().filter(|f| f.2 && is_new(f.0)).map(|f| f.0.to_string()));
            self.fields = set;
        }
        if let Some(order) = &s.order {
            // Gespeicherte Reihenfolge; neue Felder hinter ihrem Vorgänger aus der Standardreihenfolge.
            let mut keys: Vec<&'static str> = order.iter().filter_map(|k| self.field(k)).map(|f| f.0).collect();
            keys.dedup();
            for (i, f) in self.spec.fields.iter().enumerate() {
                if keys.contains(&f.0) {
                    continue;
                }
                let pos = self.spec.fields[..i]
                    .iter()
                    .rev()
                    .find_map(|p| keys.iter().position(|k| *k == p.0))
                    .map_or(0, |p| p + 1);
                keys.insert(pos, f.0);
            }
            self.order = keys;
        }
        if let Some(columns) = &s.columns {
            let mut set: HashSet<&'static str> = columns.iter().filter_map(|k| self.field(k)).map(|f| f.0).collect();
            set.extend(self.spec.fields.iter().filter(|f| f.3 && is_new(f.0)).map(|f| f.0));
            self.as_column = set;
        }
        self
    }

    /// Schlüssel der Liste (für das Formular).
    pub fn list(&self) -> &'static str {
        self.spec.key
    }

    /// Bezeichnung der Hauptangabe („Betreff“, „Namen“ …).
    pub fn main(&self) -> &'static str {
        self.spec.main
    }

    /// Ist dieser Filter sichtbar?
    pub fn filter(&self, key: &str) -> bool {
        !self.hidden_filters.contains(key)
    }

    /// Ist dieses Feld sichtbar?
    pub fn show(&self, key: &str) -> bool {
        self.fields.contains(key)
    }

    /// Ist dieses Feld sichtbar und steht unter der Hauptangabe (nicht als Spalte)?
    pub fn in_meta(&self, key: &str) -> bool {
        self.show(key) && !self.as_column.contains(key)
    }

    fn ordered(&self, column: bool) -> impl Iterator<Item = &'static Field> + '_ {
        self.order.iter().filter_map(|k| self.field(k)).filter(move |f| self.as_column.contains(f.0) == column)
    }

    /// Sichtbare Spalten in Anzeigereihenfolge.
    pub fn columns(&self) -> Vec<(&'static str, &'static str)> {
        self.ordered(true).filter(|f| self.show(f.0)).map(|f| (f.0, f.1)).collect()
    }

    /// Sichtbare Angaben unter der Hauptangabe in Anzeigereihenfolge.
    pub fn meta(&self) -> Vec<&'static str> {
        self.ordered(false).filter(|f| self.show(f.0)).map(|f| f.0).collect()
    }

    pub fn filter_choices(&self) -> Vec<Choice> {
        self.spec.filters.iter().map(|(key, label)| Choice { key, label, on: self.filter(key) }).collect()
    }

    pub fn meta_choices(&self) -> Vec<Choice> {
        self.ordered(false).map(|f| Choice { key: f.0, label: f.1, on: self.show(f.0) }).collect()
    }

    pub fn column_choices(&self) -> Vec<Choice> {
        self.ordered(true).map(|f| Choice { key: f.0, label: f.1, on: self.show(f.0) }).collect()
    }
}

/// Einstellungen eines Agenten für eine Liste (Standard, falls nichts gespeichert).
pub async fn load_for(db: &PgPool, user_id: i64, spec: &'static Spec) -> AppResult<ListPrefs> {
    let stored: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT CASE WHEN $2::text = 'tickets' THEN ticket_list ELSE lists -> $2::text END FROM user_prefs WHERE user_id = $1")
            .bind(user_id)
            .bind(spec.key)
            .fetch_optional(db)
            .await?
            .flatten();
    let prefs = ListPrefs::defaults(spec);
    Ok(match stored.and_then(|v| serde_json::from_value::<Stored>(v).ok()) {
        Some(s) => prefs.apply(s),
        None => prefs,
    })
}

/// Einstellungen der Ticketlisten.
pub async fn load(db: &PgPool, user_id: i64) -> AppResult<ListPrefs> {
    load_for(db, user_id, &TICKETS).await
}

async fn save(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Form(pairs): Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    let value = |name: &str| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    let spec: &'static Spec = SPECS.iter().copied().find(|s| Some(s.key) == value("list")).unwrap_or(&TICKETS);
    let back = value("return_to")
        .filter(|v| v.starts_with('/') && !v.starts_with("//"))
        .unwrap_or("/views")
        .to_string();
    let reset = value("reset").is_some();
    let stored = if reset {
        json!({})
    } else {
        let defaults = ListPrefs::defaults(spec);
        let checked = |name: &str| -> HashSet<&str> { pairs.iter().filter(|(k, _)| k == name).map(|(_, v)| v.as_str()).collect() };
        let filters = checked("filter");
        let fields = checked("field");
        // Reihenfolge = Reihenfolge der Felder im Formular (per Ziehen sortiert), jeweils
        // mit Bereich: „col:sla“ = Spalte, „meta:tags“ = unter der Hauptangabe.
        let placed: Vec<(&str, bool)> = pairs
            .iter()
            .filter(|(k, _)| k == "order")
            .filter_map(|(_, v)| {
                let (zone, key) = v.split_once(':').unwrap_or(("", v.as_str()));
                defaults.field(key).map(|f| (f.0, zone == "col"))
            })
            .collect();
        json!({
            "hidden_filters": spec.filters.iter().map(|f| f.0).filter(|k| !filters.contains(k)).collect::<Vec<_>>(),
            "fields": spec.fields.iter().map(|f| f.0).filter(|k| fields.contains(k)).collect::<Vec<_>>(),
            "order": placed.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            "columns": placed.iter().filter(|(_, col)| *col).map(|(k, _)| *k).collect::<Vec<_>>(),
        })
    };
    if spec.key == TICKETS.key {
        sqlx::query(
            "INSERT INTO user_prefs (user_id, ticket_list, updated_at) VALUES ($1, $2, now())
             ON CONFLICT (user_id) DO UPDATE SET ticket_list = $2, updated_at = now()",
        )
        .bind(me.id)
        .bind(stored)
        .execute(&st.db)
        .await?;
    } else {
        sqlx::query(
            "INSERT INTO user_prefs (user_id, lists, updated_at) VALUES ($1, jsonb_build_object($2::text, $3::jsonb), now())
             ON CONFLICT (user_id) DO UPDATE SET lists = user_prefs.lists || jsonb_build_object($2::text, $3::jsonb), updated_at = now()",
        )
        .bind(me.id)
        .bind(spec.key)
        .bind(stored)
        .execute(&st.db)
        .await?;
    }
    Ok(see_other_ok(&back, if reset { "Standardansicht wiederhergestellt" } else { "Ansicht angepasst" }))
}
