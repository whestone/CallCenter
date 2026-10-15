//! E-Mail-Kanal: Vorlagen, Postausgang mit SMTP-Versand, Eingang per Webhook,
//! Passwort-Zurücksetzen.
//!
//! SMTP wird unter Verwaltung → E-Mail eingerichtet (Tabelle `settings`, Schlüssel
//! `smtp.*`); ohne Eintrag dort gelten die Umgebungsvariablen aus `.env.example`.
//! Ohne Konfiguration landen Mails im Postausgang mit Status „nicht gesendet“.

use std::{collections::HashMap, time::Duration};

use chrono::Utc;
use lettre::{
    message::{header::ContentType, Mailbox},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use serde::Deserialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth,
    error::{AppError, AppResult},
    models::{Ticket, User},
    service::{self, CommentChange, NewTicket, TicketChanges},
};

pub fn public_url() -> String {
    std::env::var("PUBLIC_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:3000".into())
        .trim_end_matches('/')
        .to_string()
}

// ---------------------------------------------------------------------------
// Einstellungen & Vorlagen
// ---------------------------------------------------------------------------

pub const TEMPLATE_KEYS: [(&str, &str); 5] = [
    ("email.from_name", "Absendername"),
    ("email.signature", "Signatur"),
    ("email.reply_subject", "Betreff (Antworten & Benachrichtigungen)"),
    ("email.reply_body", "Text bei Agenten-Antwort"),
    ("email.notify_body", "Text bei Benachrichtigung (Trigger)"),
];

pub async fn settings<'e, E>(ex: E) -> AppResult<HashMap<String, String>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM settings").fetch_all(ex).await?;
    Ok(rows.into_iter().collect())
}

pub async fn save_setting(db: &PgPool, key: &str, value: &str) -> AppResult<()> {
    if !TEMPLATE_KEYS.iter().any(|(k, _)| *k == key) {
        return Err(AppError::invalid("Unbekannte Einstellung"));
    }
    sqlx::query("INSERT INTO settings (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = $2")
        .bind(key)
        .bind(value.replace("\r\n", "\n"))
        .execute(db)
        .await?;
    Ok(())
}

fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    vars.iter().fold(template.to_string(), |acc, (k, v)| acc.replace(&format!("{{{{{k}}}}}"), v))
}

// ---------------------------------------------------------------------------
// Postausgang
// ---------------------------------------------------------------------------

pub async fn enqueue<'e, E>(ex: E, ticket_id: Option<i64>, to: &str, subject: &str, body: &str) -> AppResult<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query("INSERT INTO email_outbox (ticket_id, to_email, subject, body) VALUES ($1, $2, $3, $4)")
        .bind(ticket_id)
        .bind(to.trim())
        .bind(subject)
        .bind(body)
        .execute(ex)
        .await?;
    Ok(())
}

pub enum TicketMail<'a> {
    /// Öffentliche Antwort eines Agenten.
    Reply { body: &'a str },
    /// "Anfragenden benachrichtigen" aus Triggern/Makros.
    Notification { body: &'a str },
}

/// Mail an den Anfragenden eines Tickets in den Postausgang legen (falls er eine E-Mail hat).
pub async fn queue_ticket_mail(conn: &mut PgConnection, t: &Ticket, mail: TicketMail<'_>) -> AppResult<()> {
    let requester: Option<(String, Option<String>)> = sqlx::query_as("SELECT name, email FROM users WHERE id = $1")
        .bind(t.requester_id)
        .fetch_optional(&mut *conn)
        .await?;
    // Antworten gehen zusätzlich an alle Personen in CC.
    let cc: Vec<(String, String)> = match mail {
        TicketMail::Reply { .. } if !t.cc_ids.is_empty() => {
            sqlx::query_as("SELECT name, email FROM users WHERE id = ANY($1) AND email IS NOT NULL AND active")
                .bind(&t.cc_ids)
                .fetch_all(&mut *conn)
                .await?
        }
        _ => Vec::new(),
    };
    let mut recipients: Vec<(String, String)> = match requester {
        Some((name, Some(to))) => vec![(name, to)],
        _ => Vec::new(),
    };
    recipients.extend(cc);
    for (name, to) in recipients {
        queue_one(&mut *conn, t, &mail, name, &to).await?;
    }
    Ok(())
}

async fn queue_one(conn: &mut PgConnection, t: &Ticket, mail: &TicketMail<'_>, name: String, to: &str) -> AppResult<()> {
    let s = settings(&mut *conn).await?;
    let get = |k: &str| s.get(k).cloned().unwrap_or_default();
    let (template, comment) = match mail {
        TicketMail::Reply { body } => (get("email.reply_body"), *body),
        TicketMail::Notification { body } => (get("email.notify_body"), *body),
    };
    // E-Mails sind Klartext: Formatierung (fett, Listen, Links) lesbar umsetzen.
    let comment = crate::markdown::to_plain(comment);
    let comment = comment.as_str();
    // Ohne echten Namen (nur E-Mail-Adresse bekannt) neutral anreden.
    let name = if name.contains('@') { String::new() } else { name };
    let id = t.id.to_string();
    let portal_link = format!("{}/portal/tickets/{}", public_url(), t.id);
    let signature = get("email.signature");
    let vars = [
        ("ticket.id", id.as_str()),
        ("ticket.subject", t.subject.as_str()),
        ("requester.name", name.as_str()),
        ("comment", comment),
        ("portal_link", portal_link.as_str()),
        ("signature", signature.as_str()),
    ];
    let subject = fill(&get("email.reply_subject"), &vars);
    let body = fill(&template, &vars).replace(" ,\n", ",\n");
    enqueue(&mut *conn, Some(t.id), to, &subject, &body).await
}

#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub password: Option<String>,
    pub from: String,
    /// "starttls" (Standard), "tls" (implizit, Port 465) oder "none" (nur lokale Relays)
    pub security: String,
    /// Herkunft: "admin" (Verwaltung) oder "env" (Umgebungsvariablen).
    pub source: &'static str,
}

/// Felder der SMTP-Einstellungen in der Verwaltung (Schlüssel in `settings`).
pub const SMTP_KEYS: [&str; 6] = ["smtp.host", "smtp.port", "smtp.security", "smtp.user", "smtp.password", "smtp.from"];

pub fn default_port(security: &str) -> u16 {
    match security {
        "tls" => 465,
        "none" => 25,
        _ => 587,
    }
}

fn build_smtp(var: impl Fn(&str) -> Option<String>, source: &'static str) -> Option<SmtpConfig> {
    let host = var("host")?;
    let security = var("security").unwrap_or_else(|| "starttls".into()).to_lowercase();
    Some(SmtpConfig {
        port: var("port").and_then(|p| p.parse().ok()).unwrap_or_else(|| default_port(&security)),
        user: var("user"),
        password: var("password"),
        from: var("from").unwrap_or_else(|| format!("support@{host}")),
        security,
        host,
        source,
    })
}

/// SMTP-Zugang aus der Verwaltung, ersatzweise aus den Umgebungsvariablen `SMTP_*`.
pub fn smtp_from(settings: &HashMap<String, String>) -> Option<SmtpConfig> {
    let clean = |v: Option<&String>| v.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    build_smtp(|k| clean(settings.get(&format!("smtp.{k}"))), "admin").or_else(|| {
        build_smtp(|k| clean(std::env::var(format!("SMTP_{}", k.to_uppercase())).ok().as_ref()), "env")
    })
}

pub async fn smtp_config<'e, E>(ex: E) -> AppResult<Option<SmtpConfig>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    Ok(smtp_from(&settings(ex).await?))
}

/// SMTP-Zugang speichern. Leeres Passwort = bisheriges behalten.
pub async fn save_smtp(db: &PgPool, values: &HashMap<&str, String>) -> AppResult<()> {
    let host = values.get("smtp.host").map(|v| v.trim()).unwrap_or_default();
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(AppError::invalid("Bitte einen SMTP-Server angeben"));
    }
    if let Some(port) = values.get("smtp.port").map(|v| v.trim()).filter(|v| !v.is_empty()) {
        if port.parse::<u16>().ok().filter(|p| *p > 0).is_none() {
            return Err(AppError::invalid("Port muss eine Zahl zwischen 1 und 65535 sein"));
        }
    }
    if !matches!(values.get("smtp.security").map(String::as_str).unwrap_or("starttls"), "starttls" | "tls" | "none") {
        return Err(AppError::invalid("Unbekannte Verschlüsselung"));
    }
    if let Some(from) = values.get("smtp.from").map(|v| v.trim()).filter(|v| !v.is_empty()) {
        if from.parse::<lettre::Address>().is_err() {
            return Err(AppError::invalid("Absenderadresse ist keine gültige E-Mail-Adresse"));
        }
    }
    let mut tx = db.begin().await?;
    for key in SMTP_KEYS {
        let value = values.get(key).map(|v| v.trim().to_string()).unwrap_or_default();
        if key == "smtp.password" && value.is_empty() {
            continue;
        }
        sqlx::query("INSERT INTO settings (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = $2")
            .bind(key)
            .bind(value)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// SMTP-Zugang aus der Verwaltung entfernen (danach gelten ggf. wieder die Umgebungsvariablen).
pub async fn clear_smtp(db: &PgPool) -> AppResult<()> {
    sqlx::query("DELETE FROM settings WHERE key = ANY($1)").bind(&SMTP_KEYS[..]).execute(db).await?;
    Ok(())
}

fn transport(cfg: &SmtpConfig) -> anyhow::Result<AsyncSmtpTransport<Tokio1Executor>> {
    let builder = match cfg.security.as_str() {
        "tls" => AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.host)?,
        "none" => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&cfg.host),
        _ => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.host)?,
    };
    let mut builder = builder.port(cfg.port).timeout(Some(Duration::from_secs(20)));
    if let (Some(user), Some(pass)) = (&cfg.user, &cfg.password) {
        builder = builder.credentials(Credentials::new(user.clone(), pass.clone()));
    }
    Ok(builder.build())
}

/// Verschickt wartende Mails; ohne SMTP-Konfiguration werden sie als „nicht gesendet“ markiert.
pub async fn send_pending(db: &PgPool) -> AppResult<usize> {
    let pending: Vec<(i64, String, String, String, i32)> = sqlx::query_as(
        "SELECT id, to_email, subject, body, attempts FROM email_outbox WHERE status = 'pending' ORDER BY id LIMIT 50",
    )
    .fetch_all(db)
    .await?;
    if pending.is_empty() {
        return Ok(0);
    }
    let settings = settings(db).await?;
    let Some(cfg) = smtp_from(&settings) else {
        sqlx::query("UPDATE email_outbox SET status = 'skipped', error = 'SMTP nicht konfiguriert' WHERE status = 'pending'")
            .execute(db)
            .await?;
        return Ok(0);
    };
    let mailer = transport(&cfg).map_err(AppError::Internal)?;
    let from_name = settings.get("email.from_name").cloned().unwrap_or_default();
    let mut sent = 0;
    for (id, to, subject, body, attempts) in pending {
        let result: Result<(), String> = match build_message(&from_name, &cfg.from, &to, &subject, &body) {
            Ok(msg) => mailer.send(msg).await.map(|_| ()).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(()) => {
                sent += 1;
                sqlx::query("UPDATE email_outbox SET status = 'sent', sent_at = now(), error = NULL, attempts = attempts + 1 WHERE id = $1")
                    .bind(id)
                    .execute(db)
                    .await?;
            }
            Err(e) => {
                // Nach drei Versuchen endgültig fehlgeschlagen.
                let status = if attempts + 1 >= 3 { "failed" } else { "pending" };
                tracing::warn!(mail = id, "E-Mail-Versand fehlgeschlagen: {e}");
                sqlx::query("UPDATE email_outbox SET status = $2, error = $3, attempts = attempts + 1 WHERE id = $1")
                    .bind(id)
                    .bind(status)
                    .bind(e)
                    .execute(db)
                    .await?;
            }
        }
    }
    Ok(sent)
}

fn build_message(from_name: &str, from: &str, to: &str, subject: &str, body: &str) -> anyhow::Result<Message> {
    let from: Mailbox = format!("{} <{}>", from_name.replace(['<', '>', '"'], ""), from).parse()?;
    Ok(Message::builder()
        .from(from)
        .to(to.parse()?)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(body.to_string())?)
}

pub fn spawn_sender(db: PgPool) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        loop {
            interval.tick().await;
            match send_pending(&db).await {
                Ok(n) if n > 0 => tracing::info!("{n} E-Mails versendet"),
                Ok(_) => {}
                Err(e) => tracing::error!("E-Mail-Versand: {}", e.public_message()),
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Eingang (Webhook)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct InboundEmail {
    pub from: String,
    #[serde(default)]
    pub from_name: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub text: String,
}

/// Absenderadresse aus "Max <max@x.de>" oder "max@x.de".
fn address(from: &str) -> String {
    match (from.rfind('<'), from.rfind('>')) {
        (Some(a), Some(b)) if a < b => from[a + 1..b].trim().to_lowercase(),
        _ => from.trim().to_lowercase(),
    }
}

fn ticket_ref(subject: &str) -> Option<i64> {
    let start = subject.find("[#")? + 2;
    let end = subject[start..].find(']')? + start;
    subject[start..end].parse().ok()
}

/// "Re:", "AW:", "Fwd:" usw. und fremde Ticketnummern "[#123]" entfernen.
fn clean_subject(subject: &str) -> String {
    let mut s = subject.trim().to_string();
    while let Some(a) = s.find("[#") {
        let Some(len) = s[a..].find(']') else { break };
        s.replace_range(a..=a + len, "");
    }
    loop {
        let trimmed = s.trim_start();
        let lower = trimmed.to_lowercase();
        match ["re:", "aw:", "fw:", "fwd:", "wg:"].iter().find(|p| lower.starts_with(*p)) {
            Some(p) => s = trimmed[p.len()..].to_string(),
            None => break,
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Zitierten Verlauf ("Am … schrieb", "On … wrote", "> …") abschneiden.
fn strip_quote(text: &str) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        let quote_header = (l.starts_with("Am ") && l.contains("schrieb"))
            || (l.starts_with("On ") && l.contains("wrote"))
            || l.starts_with("-----Original");
        if quote_header || l.starts_with('>') {
            break;
        }
        out.push(line);
    }
    let text = out.join("\n").trim().to_string();
    if text.is_empty() { "(leere E-Mail)".into() } else { text }
}

pub enum InboundResult {
    Reply(i64),
    Created(i64),
}

/// Eingehende Mail: Antwort auf bestehendes Ticket des Absenders oder neues Ticket.
pub async fn handle_inbound(db: &PgPool, mail: InboundEmail) -> AppResult<InboundResult> {
    let from = address(&mail.from);
    if !from.contains('@') {
        return Err(AppError::invalid("Ungültiger Absender"));
    }
    let sender = service::find_or_create_requester(db, &mail.from_name, Some(&from), None).await?;
    let body = strip_quote(&mail.text);
    let system: User = sqlx::query_as("SELECT * FROM users WHERE email = $1")
        .bind(crate::automations::SYSTEM_EMAIL)
        .fetch_one(db)
        .await?;

    let mut followup_source_id = None;
    if let Some(id) = ticket_ref(&mail.subject) {
        let existing: Option<Ticket> = sqlx::query_as("SELECT * FROM tickets WHERE id = $1").bind(id).fetch_optional(db).await?;
        let own = existing.filter(|t| t.requester_id == sender.id);
        // Antwort auf ein bereits geschlossenes Ticket → Folgeticket.
        if own.as_ref().is_some_and(|t| t.status == "closed") {
            followup_source_id = Some(id);
        }
        // Nur der Anfragende darf per Mail auf sein offenes Ticket antworten.
        if let Some(t) = own.filter(|t| t.status != "closed") {
            let changes = TicketChanges {
                comment: Some(CommentChange { body, public: true, author_id: Some(sender.id) }),
                ..Default::default()
            };
            service::update_ticket(db, &system, t.id, changes, None, "email").await?;
            return Ok(InboundResult::Reply(t.id));
        }
    }
    let subject = match clean_subject(&mail.subject).as_str() {
        "" => "(ohne Betreff)".to_string(),
        s => s.to_string(),
    };
    let t = service::create_ticket(
        db,
        &sender,
        NewTicket {
            subject,
            description: body,
            public: true,
            requester_id: sender.id,
            channel: "email".into(),
            custom_fields: serde_json::json!({}),
            followup_source_id,
            ..Default::default()
        },
        "email",
    )
    .await?;
    Ok(InboundResult::Created(t.id))
}

// ---------------------------------------------------------------------------
// Passwort zurücksetzen
// ---------------------------------------------------------------------------

/// Legt einen Einmal-Link an und verschickt ihn. Gibt nie preis, ob die Adresse existiert.
pub async fn request_password_reset(db: &PgPool, email: &str) -> AppResult<()> {
    let user: Option<User> = sqlx::query_as("SELECT * FROM users WHERE lower(email) = lower($1) AND active")
        .bind(email.trim())
        .fetch_optional(db)
        .await?;
    let Some(user) = user else { return Ok(()) };
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    sqlx::query("INSERT INTO password_resets (token_hash, user_id, expires_at) VALUES ($1, $2, now() + interval '1 hour')")
        .bind(auth::token_hash(&token))
        .bind(user.id)
        .execute(db)
        .await?;
    let link = format!("{}/password/reset/{token}", public_url());
    let body = format!(
        "Guten Tag {},\n\nüber diesen Link können Sie ein neues Passwort festlegen (1 Stunde gültig):\n\n{link}\n\nFalls Sie das nicht angefordert haben, ignorieren Sie diese E-Mail einfach.",
        user.name
    );
    enqueue(db, None, user.email.as_deref().unwrap_or_default(), "Passwort zurücksetzen", &body).await
}

/// Prüft einen Reset-Token und gibt den Nutzer zurück.
pub async fn reset_token_user(db: &PgPool, token: &str) -> AppResult<Option<User>> {
    Ok(sqlx::query_as(
        "SELECT u.* FROM password_resets r JOIN users u ON u.id = r.user_id
         WHERE r.token_hash = $1 AND r.used_at IS NULL AND r.expires_at > now() AND u.active",
    )
    .bind(auth::token_hash(token))
    .fetch_optional(db)
    .await?)
}

pub async fn complete_password_reset(db: &PgPool, token: &str, password: &str) -> AppResult<User> {
    if password.chars().count() < 8 {
        return Err(AppError::invalid("Das Passwort muss mindestens 8 Zeichen haben"));
    }
    let user = reset_token_user(db, token)
        .await?
        .ok_or_else(|| AppError::invalid("Der Link ist abgelaufen oder wurde bereits verwendet"))?;
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE users SET password_hash = $2, updated_at = now() WHERE id = $1")
        .bind(user.id)
        .bind(auth::hash_password(password)?)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE password_resets SET used_at = now() WHERE token_hash = $1")
        .bind(auth::token_hash(token))
        .execute(&mut *tx)
        .await?;
    // Alle bestehenden Sitzungen beenden.
    sqlx::query("DELETE FROM sessions WHERE user_id = $1").bind(user.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(user)
}

pub async fn outbox(db: &PgPool, limit: i64) -> AppResult<Vec<(i64, Option<i64>, String, String, String, Option<String>, chrono::DateTime<Utc>)>> {
    Ok(sqlx::query_as(
        "SELECT id, ticket_id, to_email, subject, status, error, created_at FROM email_outbox ORDER BY id DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(db)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        assert_eq!(address("Max Mustermann <Max@Acme.example>"), "max@acme.example");
        assert_eq!(ticket_ref("Re: [#42] Drucker defekt"), Some(42));
        assert_eq!(ticket_ref("Drucker defekt"), None);
        assert_eq!(strip_quote("Danke!\n\nAm 14.10.2026 schrieb Support:\n> alt"), "Danke!");
        assert_eq!(clean_subject("Re: AW: [#3] Frage zum Tarif"), "Frage zum Tarif");
        assert_eq!(clean_subject("Drucker [defekt]"), "Drucker [defekt]");
        assert_eq!(clean_subject("Drucker [defekt] [#12]"), "Drucker [defekt]");
        assert_eq!(fill("[#{{ticket.id}}] {{x}}", &[("ticket.id", "7"), ("x", "Hallo")]), "[#7] Hallo");
    }
}
