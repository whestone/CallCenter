//! Einstellungen aus der Verwaltung (Tabelle `settings`) mit Zwischenspeicher.
//!
//! Jede Einstellung ist hier einmal beschrieben (Bezeichnung, Art, Standardwert,
//! optional eine Umgebungsvariable als Rückfall). Gelesen wird synchron aus dem
//! Zwischenspeicher, der beim Start und nach jedem Speichern neu geladen wird –
//! Änderungen wirken also ohne Neustart.
//! Reihenfolge beim Lesen: Wert aus der Verwaltung → Umgebungsvariable → Standard.

use std::{collections::HashMap, sync::RwLock};

use sqlx::PgPool;

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Text,
    /// http(s)-Adresse ohne abschließenden Schrägstrich.
    Url,
    Number { min: i64, max: i64 },
    Bool,
    /// Wird nie angezeigt; leeres Feld beim Speichern = unverändert.
    Secret,
}

pub struct Setting {
    pub key: &'static str,
    pub label: &'static str,
    /// Abschnitt auf der Einstellungsseite.
    pub section: &'static str,
    pub kind: Kind,
    pub default: &'static str,
    pub env: Option<&'static str>,
    /// Einheit hinter dem Eingabefeld (z. B. „Min.“).
    pub unit: &'static str,
}

const fn s(
    section: &'static str,
    key: &'static str,
    label: &'static str,
    kind: Kind,
    default: &'static str,
    env: Option<&'static str>,
    unit: &'static str,
) -> Setting {
    Setting { key, label, section, kind, default, env, unit }
}

/// Abschnitte der Seite „Einstellungen“ (Allgemein) in Anzeigereihenfolge.
pub const GENERAL_SECTIONS: [&str; 4] = ["Unternehmen", "Tickets", "Kundenportal", "Sicherheit"];

pub static SETTINGS: &[Setting] = &[
    // Allgemein
    s("Unternehmen", "company.name", "Name des Helpdesks", Kind::Text, "CallCenter", None, ""),
    s("Unternehmen", "app.public_url", "Öffentliche Adresse", Kind::Url, "http://127.0.0.1:3000", Some("PUBLIC_URL"), ""),
    s("Unternehmen", "phone.country_code", "Landesvorwahl", Kind::Number { min: 1, max: 999 }, "49", Some("DEFAULT_COUNTRY_CODE"), ""),
    s("Tickets", "tickets.per_page", "Tickets pro Seite", Kind::Number { min: 10, max: 200 }, "50", None, ""),
    s("Tickets", "sla.warn_minutes", "SLA-Warnung vor Ablauf", Kind::Number { min: 1, max: 1440 }, "30", Some("SLA_WARN_MINUTES"), "Min."),
    s("Tickets", "routing.skill_timeout_minutes", "Warten auf passende Fähigkeit", Kind::Number { min: 1, max: 1440 }, "30", Some("SKILL_TIMEOUT_MINUTES"), "Min."),
    s("Tickets", "attachments.max_mb", "Max. Größe je Anhang", Kind::Number { min: 1, max: 25 }, "10", None, "MB"),
    s("Tickets", "attachments.max_count", "Max. Anhänge je Nachricht", Kind::Number { min: 1, max: 10 }, "5", None, ""),
    s("Kundenportal", "portal.registration", "Kunden dürfen sich selbst registrieren", Kind::Bool, "true", None, ""),
    s("Kundenportal", "portal.customer_solve", "Kunden dürfen Anfragen als gelöst markieren", Kind::Bool, "true", None, ""),
    s("Kundenportal", "portal.satisfaction", "Zufriedenheitsbewertung nach Lösung", Kind::Bool, "true", None, ""),
    s("Sicherheit", "auth.session_days", "Angemeldet bleiben", Kind::Number { min: 1, max: 90 }, "14", None, "Tage"),
    // Telefonie (eigene Seite)
    s("Telefonie", "telephony.secret", "Webhook-Geheimnis", Kind::Secret, "", Some("TELEPHONY_SECRET"), ""),
    s("Telefonie", "sipgate.token_id", "sipgate Token-ID", Kind::Text, "", Some("SIPGATE_TOKEN_ID"), ""),
    s("Telefonie", "sipgate.token", "sipgate Token", Kind::Secret, "", Some("SIPGATE_TOKEN"), ""),
];

static CACHE: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

pub fn setting(key: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|s| s.key == key)
}

/// Zwischenspeicher aus der Datenbank neu laden (beim Start und nach jedem Speichern).
pub async fn reload(db: &PgPool) -> AppResult<()> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM settings").fetch_all(db).await?;
    if let Ok(mut c) = CACHE.write() {
        *c = Some(rows.into_iter().collect());
    }
    Ok(())
}

fn stored(key: &str) -> Option<String> {
    CACHE.read().ok()?.as_ref()?.get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn from_env(key: &str) -> Option<String> {
    let var = setting(key)?.env?;
    std::env::var(var).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Herkunft des wirksamen Werts: "admin", "env" oder "default".
pub fn source(key: &str) -> &'static str {
    if stored(key).is_some() {
        "admin"
    } else if from_env(key).is_some() {
        "env"
    } else {
        "default"
    }
}

/// Wirksamer Wert (Verwaltung → Umgebungsvariable → Standard).
pub fn get(key: &str) -> String {
    stored(key).or_else(|| from_env(key)).unwrap_or_else(|| setting(key).map(|s| s.default.to_string()).unwrap_or_default())
}

/// Zahl, auf den erlaubten Bereich begrenzt; ungültige Werte → Standard.
pub fn int(key: &str) -> i64 {
    let Some(def) = setting(key) else { return 0 };
    let Kind::Number { min, max } = def.kind else { return 0 };
    let fallback = def.default.parse().unwrap_or(min);
    get(key).parse::<i64>().map(|v| v.clamp(min, max)).unwrap_or(fallback)
}

pub fn flag(key: &str) -> bool {
    matches!(get(key).as_str(), "true" | "1" | "on" | "yes")
}

/// Nicht leerer Wert oder `None` (für optionale Zugangsdaten).
pub fn opt(key: &str) -> Option<String> {
    Some(get(key)).filter(|v| !v.is_empty())
}

/// Eingabe prüfen und normalisieren.
fn normalize(def: &Setting, raw: &str) -> AppResult<String> {
    let v = raw.trim();
    Ok(match def.kind {
        Kind::Text | Kind::Secret => v.to_string(),
        Kind::Bool => if matches!(v, "true" | "on" | "1") { "true" } else { "false" }.to_string(),
        Kind::Url => {
            if !v.is_empty() && !(v.starts_with("http://") || v.starts_with("https://")) || v.contains(char::is_whitespace) {
                return Err(AppError::invalid(format!("{}: bitte eine Adresse mit http:// oder https:// angeben", def.label)));
            }
            v.trim_end_matches('/').to_string()
        }
        Kind::Number { min, max } => {
            if v.is_empty() {
                String::new()
            } else {
                match v.parse::<i64>() {
                    Ok(n) if (min..=max).contains(&n) => n.to_string(),
                    _ => return Err(AppError::invalid(format!("{}: Zahl zwischen {min} und {max}", def.label))),
                }
            }
        }
    })
}

/// Alle Einstellungen eines Abschnitts aus einem Formular speichern. Nicht angehakte
/// Schalter gelten als „aus“, leere Geheimnisse bleiben unverändert, leere Zahlen/Texte
/// fallen auf Umgebungsvariable bzw. Standard zurück.
pub async fn save_section(db: &PgPool, sections: &[&str], form: &[(String, String)]) -> AppResult<()> {
    let value = |k: &str| form.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    let mut changes: Vec<(&str, Option<String>)> = Vec::new();
    for def in SETTINGS.iter().filter(|s| sections.contains(&s.section)) {
        let raw = match (def.kind, value(def.key)) {
            (Kind::Bool, v) => v.unwrap_or("false"),
            (_, Some(v)) => v,
            (_, None) => continue,
        };
        let v = normalize(def, raw)?;
        match def.kind {
            Kind::Secret if v.is_empty() => {}
            _ if v.is_empty() => changes.push((def.key, None)),
            _ => changes.push((def.key, Some(v))),
        }
    }
    let mut tx = db.begin().await?;
    for (key, v) in changes {
        match v {
            Some(v) => {
                sqlx::query("INSERT INTO settings (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = $2")
                    .bind(key)
                    .bind(v)
                    .execute(&mut *tx)
                    .await?;
            }
            None => {
                sqlx::query("DELETE FROM settings WHERE key = $1").bind(key).execute(&mut *tx).await?;
            }
        }
    }
    tx.commit().await?;
    reload(db).await
}

/// Einzelnen Wert setzen bzw. (mit `None`) entfernen.
pub async fn set(db: &PgPool, key: &str, value: Option<&str>) -> AppResult<()> {
    match value {
        Some(v) => {
            sqlx::query("INSERT INTO settings (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = $2")
                .bind(key)
                .bind(v)
                .execute(db)
                .await?;
        }
        None => {
            sqlx::query("DELETE FROM settings WHERE key = $1").bind(key).execute(db).await?;
        }
    }
    reload(db).await
}

/// Zufälliges Geheimnis (64 Hex-Zeichen).
pub fn random_secret() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Darstellung eines Felds auf der Einstellungsseite.
pub struct FieldView {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: &'static str,
    /// Gespeicherter Wert (leer = nicht gesetzt; bei Geheimnissen immer leer).
    pub value: String,
    /// Wirksamer Wert als Platzhalter, wenn nichts gespeichert ist.
    pub placeholder: String,
    pub on: bool,
    pub min: i64,
    pub max: i64,
    pub unit: &'static str,
    pub from_env: bool,
}

pub fn fields(section: &str) -> Vec<FieldView> {
    SETTINGS
        .iter()
        .filter(|s| s.section == section)
        .map(|s| {
            let (kind, min, max) = match s.kind {
                Kind::Text => ("text", 0, 0),
                Kind::Url => ("url", 0, 0),
                Kind::Number { min, max } => ("number", min, max),
                Kind::Bool => ("bool", 0, 0),
                Kind::Secret => ("secret", 0, 0),
            };
            let secret = s.kind == Kind::Secret;
            let has_value = stored(s.key).is_some();
            FieldView {
                key: s.key,
                label: s.label,
                kind,
                value: if secret { String::new() } else { stored(s.key).unwrap_or_default() },
                placeholder: if secret {
                    if has_value { "gespeichert – leer lassen = unverändert".into() } else { String::new() }
                } else {
                    get(s.key)
                },
                on: flag(s.key),
                min,
                max,
                unit: s.unit,
                from_env: source(s.key) == "env",
            }
        })
        .collect()
}

/// Name des Helpdesks (Fenstertitel, Anmeldung, Zwei-Faktor-App).
pub fn brand() -> String {
    get("company.name")
}

/// Kürzel für das Logo (bis zu zwei Anfangsbuchstaben des Namens).
pub fn brand_initials() -> String {
    let name = brand();
    let words: Vec<&str> = name.split_whitespace().collect();
    let letters: String = match words.as_slice() {
        // Ein Wort: Großbuchstaben („CallCenter“ → CC), sonst die ersten zwei Zeichen.
        [one] => {
            let caps: String = one.chars().filter(|c| c.is_uppercase()).take(2).collect();
            if caps.chars().count() == 2 { caps } else { one.chars().filter(|c| c.is_alphanumeric()).take(2).collect() }
        }
        many => many.iter().filter_map(|w| w.chars().next()).take(2).collect(),
    };
    if letters.is_empty() { "CC".into() } else { letters.to_uppercase() }
}
