//! Konfigurierbare Ticket-Status: Bezeichnung, Farbe, Reihenfolge, aktiv und erlaubte
//! Übergänge (Verwaltung → Tickets → Status). Gelesen wird aus einem Zwischenspeicher,
//! der beim Start und nach jedem Speichern neu geladen wird.
//!
//! Die Schlüssel (new, open, pending, hold, solved, closed) sind fest: Regeln, SLA,
//! Berichte, Portal und Auto-Modus richten sich nach ihnen.

use std::{
    collections::HashSet,
    sync::RwLock,
    time::{SystemTime, UNIX_EPOCH},
};

use sqlx::PgPool;

use crate::{
    error::{AppError, AppResult},
    models::STATUSES,
};

#[derive(Debug, Clone)]
pub struct StatusDef {
    pub key: &'static str,
    pub label: &'static str,
    pub color: String,
    pub active: bool,
}

struct Cache {
    list: Vec<StatusDef>,
    transitions: HashSet<(String, String)>,
    version: String,
}

static CACHE: RwLock<Option<Cache>> = RwLock::new(None);

fn fixed_key(key: &str) -> Option<&'static str> {
    STATUSES.iter().copied().find(|k| *k == key)
}

/// Feste Standardbezeichnung (Rückfall, falls die Tabelle fehlt).
fn default_label(s: &str) -> &'static str {
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

pub async fn reload(db: &PgPool) -> AppResult<()> {
    let rows: Vec<(String, String, String, bool)> =
        sqlx::query_as("SELECT key, label, color, active FROM ticket_statuses ORDER BY position, key").fetch_all(db).await?;
    let list = rows
        .into_iter()
        .filter_map(|(key, label, color, active)| {
            // Bezeichnungen werden selten geändert; als &'static str sind sie überall nutzbar.
            Some(StatusDef { key: fixed_key(&key)?, label: Box::leak(label.into_boxed_str()), color, active })
        })
        .collect();
    let transitions: HashSet<(String, String)> =
        sqlx::query_as::<_, (String, String)>("SELECT from_key, to_key FROM status_transitions").fetch_all(db).await?.into_iter().collect();
    let version = format!("{:x}", SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0));
    if let Ok(mut c) = CACHE.write() {
        *c = Some(Cache { list, transitions, version });
    }
    Ok(())
}

/// Alle Status in eingestellter Reihenfolge.
pub fn all() -> Vec<StatusDef> {
    CACHE
        .read()
        .ok()
        .and_then(|c| c.as_ref().map(|c| c.list.clone()))
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| {
            STATUSES
                .iter()
                .map(|k| StatusDef { key: k, label: default_label(k), color: "#64748b".into(), active: true })
                .collect()
        })
}

pub fn label(key: &str) -> &'static str {
    all().into_iter().find(|s| s.key == key).map(|s| s.label).unwrap_or_else(|| default_label(key))
}

pub fn is_active(key: &str) -> bool {
    all().into_iter().find(|s| s.key == key).is_none_or(|s| s.active)
}

/// Ist der Wechsel erlaubt? Gleicher Status ist immer erlaubt.
pub fn allowed(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    CACHE.read().ok().and_then(|c| c.as_ref().map(|c| c.transitions.contains(&(from.to_string(), to.to_string())))).unwrap_or(true)
}

/// Prüft einen manuellen Statuswechsel (Oberfläche) und liefert eine verständliche Meldung.
pub fn check_transition(from: &str, to: &str) -> AppResult<()> {
    if !allowed(from, to) {
        return Err(AppError::invalid(format!("Der Wechsel von „{}“ nach „{}“ ist nicht erlaubt", label(from), label(to))));
    }
    if from != to && !is_active(to) {
        return Err(AppError::invalid(format!("Der Status „{}“ ist deaktiviert", label(to))));
    }
    Ok(())
}

/// Ziele für die Auswahl im Ticket: aktueller Status plus alle erlaubten, aktiven Ziele.
pub fn targets(from: &str) -> Vec<&'static str> {
    all().into_iter().filter(|s| s.key == from || (s.active && allowed(from, s.key))).map(|s| s.key).collect()
}

/// Aktive Status (in Reihenfolge), optional eingeschränkt auf eine Auswahl.
pub fn active_keys(only: Option<&[&str]>) -> Vec<&'static str> {
    all().into_iter().filter(|s| s.active && only.is_none_or(|o| o.contains(&s.key))).map(|s| s.key).collect()
}

/// Kennung für die Stylesheet-Adresse (ändert sich nach jedem Speichern).
pub fn version() -> String {
    CACHE.read().ok().and_then(|c| c.as_ref().map(|c| c.version.clone())).unwrap_or_default()
}

fn valid_color(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit())
}

/// Stylesheet mit den eingestellten Farben (Statuspillen, Punkte, Auswahl) – für hell
/// und dunkel aus derselben Farbe abgeleitet.
pub fn css() -> String {
    let mut out = String::from("/* Statusfarben aus Verwaltung → Tickets → Status */\n");
    for s in all() {
        let c = if valid_color(&s.color) { s.color.clone() } else { "#64748b".into() };
        let k = s.key;
        out.push_str(&format!(
            ".status-pill.s-{k}, .status-badge.s-{k} {{ background: color-mix(in srgb, {c} 15%, var(--panel)); color: color-mix(in srgb, {c} 72%, var(--text)); border-color: color-mix(in srgb, {c} 38%, var(--panel)); }}\n\
             .combo-dot.st-{k}, .dot.s-{k}, .status-swatch.s-{k} {{ background: {c}; }}\n"
        ));
    }
    out
}

/// Einstellungen speichern: (Schlüssel, Bezeichnung, Farbe, aktiv) in Listenreihenfolge
/// und die erlaubten Übergänge.
pub async fn save(db: &PgPool, rows: &[(String, String, String, bool)], transitions: &[(String, String)]) -> AppResult<()> {
    for (key, label, color, _) in rows {
        if fixed_key(key).is_none() {
            return Err(AppError::invalid(format!("Unbekannter Status: {key}")));
        }
        if label.trim().is_empty() {
            return Err(AppError::invalid("Jeder Status braucht eine Bezeichnung"));
        }
        if !valid_color(color) {
            return Err(AppError::invalid(format!("Ungültige Farbe für „{label}“")));
        }
    }
    if !rows.iter().any(|(k, _, _, active)| *active && (k == "open" || k == "new")) {
        return Err(AppError::invalid("„Neu“ oder „Offen“ muss aktiv bleiben"));
    }
    let mut tx = db.begin().await?;
    for (i, (key, label, color, active)) in rows.iter().enumerate() {
        sqlx::query("UPDATE ticket_statuses SET label = $2, color = $3, position = $4, active = $5 WHERE key = $1")
            .bind(key)
            .bind(label.trim())
            .bind(color)
            .bind(i as i32 + 1)
            .bind(active)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM status_transitions").execute(&mut *tx).await?;
    for (from, to) in transitions {
        if from != to && fixed_key(from).is_some() && fixed_key(to).is_some() {
            sqlx::query("INSERT INTO status_transitions (from_key, to_key) VALUES ($1, $2) ON CONFLICT DO NOTHING")
                .bind(from)
                .bind(to)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    reload(db).await
}
