//! Konfigurierbare Ticket-Status (Verwaltung → Tickets → Status): Bezeichnung, Farbe,
//! Reihenfolge, aktiv, erlaubte Übergänge – und eigene Status.
//!
//! Jeder Status hat eine Grundart (new, open, pending, hold, solved, closed). Die sechs
//! Grundstatus sind fest; eigene Status (z. B. „Warten auf Lieferant“, Grundart „hold“)
//! liegen darüber: Das Ticket behält in `status` die Grundart – Regeln, SLA, Berichte,
//! Portal und Auto-Modus arbeiten damit unverändert – und trägt den eigenen Status in
//! `substatus`. Anzeige, Auswahl, Filter, Übergänge und SLA-Pause nutzen den angezeigten
//! Status (eigener Status, sonst Grundart).
//!
//! Gelesen wird aus einem Zwischenspeicher (Start und nach jedem Speichern).

use std::{
    collections::HashSet,
    sync::RwLock,
    time::{SystemTime, UNIX_EPOCH},
};

use sqlx::PgPool;

use crate::{
    error::{AppError, AppResult},
    models::{Ticket, STATUSES},
};

#[derive(Debug, Clone)]
pub struct StatusDef {
    pub key: &'static str,
    pub label: &'static str,
    pub color: String,
    pub active: bool,
    /// Grundart (einer der sechs festen Status).
    pub category: &'static str,
    /// Selbst angelegt (löschbar, Grundart wählbar).
    pub custom: bool,
}

struct Cache {
    list: Vec<StatusDef>,
    transitions: HashSet<(String, String)>,
    version: String,
}

static CACHE: RwLock<Option<Cache>> = RwLock::new(None);

fn base_key(key: &str) -> Option<&'static str> {
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

/// Zeichenkette dauerhaft ablegen (Bezeichnungen/Schlüssel ändern sich selten; als
/// `&'static str` lassen sie sich überall wie die festen Werte verwenden).
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

pub async fn reload(db: &PgPool) -> AppResult<()> {
    let rows: Vec<(String, String, String, bool, String, bool)> =
        sqlx::query_as("SELECT key, label, color, active, category, custom FROM ticket_statuses ORDER BY position, key")
            .fetch_all(db)
            .await?;
    let list = rows
        .into_iter()
        .filter_map(|(key, label, color, active, category, custom)| {
            Some(StatusDef {
                key: base_key(&key).unwrap_or_else(|| leak(key)),
                label: leak(label),
                color,
                active,
                category: base_key(&category)?,
                custom,
            })
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
                .map(|k| StatusDef { key: k, label: default_label(k), color: "#64748b".into(), active: true, category: k, custom: false })
                .collect()
        })
}

fn find(key: &str) -> Option<StatusDef> {
    all().into_iter().find(|s| s.key == key)
}

pub fn label(key: &str) -> &'static str {
    find(key).map(|s| s.label).unwrap_or_else(|| default_label(key))
}

/// Grundart eines (auch eigenen) Status.
pub fn category(key: &str) -> Option<&'static str> {
    base_key(key).or_else(|| find(key).map(|s| s.category))
}

pub fn is_custom(key: &str) -> bool {
    find(key).is_some_and(|s| s.custom)
}

pub fn is_active(key: &str) -> bool {
    find(key).is_none_or(|s| s.active)
}

/// Angezeigter Status eines Tickets: eigener Status, sonst die Grundart.
pub fn shown(t: &Ticket) -> &str {
    t.substatus.as_deref().unwrap_or(&t.status)
}

/// Grundart und eigener Status für einen gewählten (angezeigten) Status.
pub fn resolve(key: &str) -> AppResult<(&'static str, Option<String>)> {
    match find(key) {
        Some(s) if s.custom => Ok((s.category, Some(s.key.to_string()))),
        Some(s) => Ok((s.category, None)),
        None => base_key(key).map(|k| (k, None)).ok_or_else(|| AppError::invalid(format!("Unbekannter Status: {key}"))),
    }
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

/// Aktive Status (in Reihenfolge), deren Grundart in `categories` liegt (alle bei `None`).
pub fn active_keys(categories: Option<&[&str]>) -> Vec<&'static str> {
    all().into_iter().filter(|s| s.active && categories.is_none_or(|o| o.contains(&s.category))).map(|s| s.key).collect()
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

/// Einstellungen speichern: (Schlüssel, Bezeichnung, Farbe, aktiv, Grundart) in
/// Listenreihenfolge und die erlaubten Übergänge.
pub async fn save(db: &PgPool, rows: &[(String, String, String, bool, String)], transitions: &[(String, String)]) -> AppResult<()> {
    let known: HashSet<&'static str> = all().into_iter().map(|s| s.key).collect();
    for (key, label, color, _, category) in rows {
        if !known.contains(key.as_str()) {
            return Err(AppError::invalid(format!("Unbekannter Status: {key}")));
        }
        if label.trim().is_empty() {
            return Err(AppError::invalid("Jeder Status braucht eine Bezeichnung"));
        }
        if !valid_color(color) {
            return Err(AppError::invalid(format!("Ungültige Farbe für „{}“", label.trim())));
        }
        if base_key(category).is_none() {
            return Err(AppError::invalid(format!("Ungültige Grundart für „{}“", label.trim())));
        }
    }
    if !rows.iter().any(|(k, _, _, active, _)| *active && (k == "open" || k == "new")) {
        return Err(AppError::invalid("„Neu“ oder „Offen“ muss aktiv bleiben"));
    }
    let mut tx = db.begin().await?;
    for (i, (key, label, color, active, category)) in rows.iter().enumerate() {
        // Die Grundart der festen Status bleibt fest.
        sqlx::query(
            "UPDATE ticket_statuses SET label = $2, color = $3, position = $4, active = $5,
                    category = CASE WHEN custom THEN $6 ELSE category END
             WHERE key = $1",
        )
        .bind(key)
        .bind(label.trim())
        .bind(color)
        .bind(i as i32 + 1)
        .bind(active)
        .bind(category)
        .execute(&mut *tx)
        .await?;
        // Grundart eines eigenen Status geändert → betroffene Tickets mitziehen.
        sqlx::query("UPDATE tickets SET status = $2 WHERE substatus = $1 AND status <> $2 AND status <> 'closed'")
            .bind(key)
            .bind(category)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM status_transitions").execute(&mut *tx).await?;
    for (from, to) in transitions {
        if from != to && known.contains(from.as_str()) && known.contains(to.as_str()) {
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

/// Eigenen Status anlegen. Er darf zunächst aus und in alle Status wechseln, in die
/// bzw. aus denen seine Grundart darf.
pub async fn create(db: &PgPool, label: &str, category: &str, color: &str) -> AppResult<String> {
    let label = label.trim();
    if label.is_empty() {
        return Err(AppError::invalid("Bitte eine Bezeichnung angeben"));
    }
    let Some(category) = base_key(category) else { return Err(AppError::invalid("Bitte eine Grundart wählen")) };
    if !valid_color(color) {
        return Err(AppError::invalid("Ungültige Farbe"));
    }
    if all().iter().any(|s| s.label.eq_ignore_ascii_case(label)) {
        return Err(AppError::invalid(format!("Einen Status „{label}“ gibt es schon")));
    }
    // Schlüssel aus der Bezeichnung: nur a–z, 0–9 und _, mit Präfix gegen Kollisionen.
    let slug: String = label
        .to_lowercase()
        .replace('ä', "ae")
        .replace('ö', "oe")
        .replace('ü', "ue")
        .replace('ß', "ss")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    let base = format!("c_{}", if slug.is_empty() { "status".into() } else { slug.chars().take(30).collect::<String>() });
    let mut key = base.clone();
    let mut n = 2;
    while all().iter().any(|s| s.key == key) {
        key = format!("{base}_{n}");
        n += 1;
    }
    let mut tx = db.begin().await?;
    sqlx::query(
        "INSERT INTO ticket_statuses (key, label, color, position, active, category, custom)
         VALUES ($1, $2, $3, (SELECT coalesce(max(position), 0) + 1 FROM ticket_statuses), TRUE, $4, TRUE)",
    )
    .bind(&key)
    .bind(label)
    .bind(color)
    .bind(category)
    .execute(&mut *tx)
    .await?;
    // Übergänge der Grundart übernehmen (in beide Richtungen) und mit der Grundart selbst verbinden.
    sqlx::query(
        "INSERT INTO status_transitions (from_key, to_key)
         SELECT $1::text, to_key FROM status_transitions WHERE from_key = $2::text
         UNION SELECT from_key, $1::text FROM status_transitions WHERE to_key = $2::text
         UNION SELECT $1::text, $2::text WHERE $2::text <> 'closed'
         UNION SELECT $2::text, $1::text WHERE $2::text <> 'closed'
         ON CONFLICT DO NOTHING",
    )
    .bind(&key)
    .bind(category)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM status_transitions WHERE from_key = to_key").execute(&mut *tx).await?;
    tx.commit().await?;
    reload(db).await?;
    Ok(key)
}

/// Eigenen Status löschen; betroffene Tickets behalten ihre Grundart.
pub async fn delete(db: &PgPool, key: &str) -> AppResult<()> {
    if !is_custom(key) {
        return Err(AppError::invalid("Die festen Status können nicht gelöscht werden – nur deaktiviert"));
    }
    sqlx::query("DELETE FROM ticket_statuses WHERE key = $1 AND custom").bind(key).execute(db).await?;
    reload(db).await
}
