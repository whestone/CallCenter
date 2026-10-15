//! E-Mail-Eingang per IMAP-Abruf: holt ungelesene Mails aus einem Postfach und
//! macht daraus Tickets bzw. Antworten (gleiche Logik wie der Eingangs-Webhook).
//!
//! Bewusst ein schlanker eigener Client (LOGIN, SELECT, UID SEARCH/FETCH/STORE),
//! Zugang unter Verwaltung → E-Mail (Tabelle `settings`, Schlüssel `imap.*`), ersatzweise
//! `IMAP_*` aus der `.env`. Änderungen wirken ohne Neustart. Verarbeitete Mails werden
//! als gelesen markiert.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, Utc};
use mail_parser::{MessageParser, MimeHeaders};
use sqlx::PgPool;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf},
    net::TcpStream,
};
use tokio_rustls::{
    rustls::{self, pki_types::ServerName},
    TlsConnector,
};

use crate::{
    email::{self, InboundEmail, InboundResult},
    error::{AppError, AppResult},
    models::User,
    service::{self, Upload},
};

#[derive(Debug, Clone, PartialEq)]
pub struct ImapConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub folder: String,
    /// "tls" (implizit, Port 993, Standard) oder "none" (nur für lokale Tests)
    pub security: String,
    pub interval: u64,
    /// Herkunft: "admin" (Verwaltung) oder "env" (Umgebungsvariablen).
    pub source: &'static str,
}

/// Felder des IMAP-Zugangs in der Verwaltung (Schlüssel in `settings`).
pub const KEYS: [&str; 7] =
    ["imap.host", "imap.port", "imap.security", "imap.user", "imap.password", "imap.folder", "imap.interval"];

pub fn default_port(security: &str) -> u16 {
    if security == "none" {
        143
    } else {
        993
    }
}

fn build(var: impl Fn(&str) -> Option<String>, source: &'static str) -> Option<ImapConfig> {
    let security = var("security").unwrap_or_else(|| "tls".into()).to_lowercase();
    Some(ImapConfig {
        host: var("host")?,
        port: var("port").and_then(|p| p.parse().ok()).unwrap_or_else(|| default_port(&security)),
        user: var("user")?,
        password: var("password")?,
        folder: var("folder").unwrap_or_else(|| "INBOX".into()),
        interval: var("interval").and_then(|p| p.parse().ok()).filter(|s| *s >= 10).unwrap_or(60),
        security,
        source,
    })
}

/// IMAP-Zugang aus der Verwaltung, ersatzweise aus den Umgebungsvariablen `IMAP_*`.
pub fn config_from(settings: &HashMap<String, String>) -> Option<ImapConfig> {
    let clean = |v: Option<&String>| v.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    build(|k| clean(settings.get(&format!("imap.{k}"))), "admin").or_else(|| {
        let env_key = |k: &str| if k == "interval" { "IMAP_POLL_SECONDS".to_string() } else { format!("IMAP_{}", k.to_uppercase()) };
        build(|k| clean(std::env::var(env_key(k)).ok().as_ref()), "env")
    })
}

pub async fn current(db: &PgPool) -> AppResult<Option<ImapConfig>> {
    Ok(config_from(&email::settings(db).await?))
}

/// IMAP-Zugang speichern (leeres Passwort = bisheriges behalten) und gleich einen Abruf anstoßen.
pub async fn save(db: &PgPool, values: &HashMap<&str, String>) -> AppResult<()> {
    let get = |k: &str| values.get(k).map(|v| v.trim()).unwrap_or_default();
    if get("imap.host").is_empty() || get("imap.host").contains(char::is_whitespace) {
        return Err(AppError::invalid("Bitte einen IMAP-Server angeben"));
    }
    if get("imap.user").is_empty() {
        return Err(AppError::invalid("Bitte einen Benutzer angeben"));
    }
    if !get("imap.port").is_empty() && get("imap.port").parse::<u16>().ok().filter(|p| *p > 0).is_none() {
        return Err(AppError::invalid("Port muss eine Zahl zwischen 1 und 65535 sein"));
    }
    if !matches!(get("imap.security"), "" | "tls" | "none") {
        return Err(AppError::invalid("Unbekannte Verschlüsselung"));
    }
    if !get("imap.interval").is_empty() && get("imap.interval").parse::<u64>().ok().filter(|s| *s >= 10).is_none() {
        return Err(AppError::invalid("Abrufintervall: mindestens 10 Sekunden"));
    }
    let has_password: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM settings WHERE key = 'imap.password' AND value <> '')")
        .fetch_one(db)
        .await?;
    if get("imap.password").is_empty() && !has_password {
        return Err(AppError::invalid("Bitte ein Passwort angeben"));
    }
    email::store_settings(db, &KEYS, values, Some("imap.password")).await?;
    request_poll();
    Ok(())
}

pub async fn clear(db: &PgPool) -> AppResult<()> {
    email::delete_settings(db, &KEYS).await?;
    request_poll();
    Ok(())
}

static POLL_NOW: AtomicBool = AtomicBool::new(false);

/// Beim nächsten Durchlauf der Hintergrundschleife sofort abrufen.
pub fn request_poll() {
    POLL_NOW.store(true, Ordering::Relaxed);
}

/// Ergebnis des letzten Abrufs (für die Anzeige in der Verwaltung).
#[derive(Debug, Clone)]
pub struct Status {
    pub at: DateTime<Utc>,
    pub ok: bool,
    pub message: String,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);

pub fn last_status() -> Option<Status> {
    STATUS.lock().ok().and_then(|s| s.clone())
}

fn set_status(ok: bool, message: String) {
    if let Ok(mut s) = STATUS.lock() {
        *s = Some(Status { at: Utc::now(), ok, message });
    }
}

fn clear_status() {
    if let Ok(mut s) = STATUS.lock() {
        *s = None;
    }
}

// ---------------------------------------------------------------------------
// Protokoll
// ---------------------------------------------------------------------------

/// Eine Antwortzeile des Servers samt eingebetteter Literale (`{n}` + n Bytes).
type Line = (String, Vec<Vec<u8>>);

struct Session<S> {
    reader: BufReader<ReadHalf<S>>,
    writer: WriteHalf<S>,
    tag: u32,
}

fn literal_len(line: &str) -> Option<usize> {
    let rest = line.strip_suffix('}')?;
    let start = rest.rfind('{')?;
    rest[start + 1..].trim_end_matches('+').parse().ok()
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

impl<S: AsyncRead + AsyncWrite + Unpin> Session<S> {
    fn new(stream: S) -> Self {
        let (r, w) = tokio::io::split(stream);
        Session { reader: BufReader::new(r), writer: w, tag: 0 }
    }

    async fn read_line(&mut self) -> anyhow::Result<Line> {
        let mut text = String::new();
        let mut literals = Vec::new();
        loop {
            let mut buf = Vec::new();
            let n = tokio::time::timeout(Duration::from_secs(60), self.reader.read_until(b'\n', &mut buf))
                .await
                .context("Zeitüberschreitung beim Lesen")??;
            if n == 0 {
                bail!("Verbindung vom Server geschlossen");
            }
            let part = String::from_utf8_lossy(&buf);
            let part = part.trim_end_matches(['\r', '\n']);
            text.push_str(part);
            match literal_len(part) {
                Some(len) => {
                    let mut data = vec![0; len];
                    self.reader.read_exact(&mut data).await?;
                    literals.push(data);
                }
                None => return Ok((text, literals)),
            }
        }
    }

    /// Befehl senden und alle Antwortzeilen bis zur getaggten Abschlusszeile lesen.
    async fn command(&mut self, cmd: &str) -> anyhow::Result<Vec<Line>> {
        self.tag += 1;
        let tag = format!("A{:03}", self.tag);
        self.writer.write_all(format!("{tag} {cmd}\r\n").as_bytes()).await?;
        self.writer.flush().await?;
        let verb = cmd.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
        let mut untagged = Vec::new();
        loop {
            let (line, literals) = self.read_line().await?;
            if let Some(rest) = line.strip_prefix(&format!("{tag} ")) {
                if rest.starts_with("OK") {
                    return Ok(untagged);
                }
                // Das Passwort aus LOGIN taucht hier bewusst nicht auf.
                let verb = if verb.starts_with("LOGIN") { "LOGIN".to_string() } else { verb };
                bail!("{verb}: {rest}");
            }
            untagged.push((line, literals));
        }
    }
}

fn tls_connector() -> anyhow::Result<TlsConnector> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

// ---------------------------------------------------------------------------
// Abruf
// ---------------------------------------------------------------------------

/// Höchstens so viele Mails pro Durchlauf (der Rest folgt beim nächsten).
const BATCH: usize = 25;

pub async fn poll(db: &PgPool, cfg: &ImapConfig) -> anyhow::Result<usize> {
    let tcp = tokio::time::timeout(Duration::from_secs(20), TcpStream::connect((cfg.host.as_str(), cfg.port)))
        .await
        .context("Zeitüberschreitung beim Verbinden")?
        .with_context(|| format!("Verbindung zu {}:{} fehlgeschlagen", cfg.host, cfg.port))?;
    if cfg.security == "none" {
        return session(db, cfg, tcp).await;
    }
    let domain = ServerName::try_from(cfg.host.clone()).map_err(|_| anyhow!("Ungültiger Hostname"))?;
    let tls = tls_connector()?.connect(domain, tcp).await.context("TLS-Verbindung fehlgeschlagen")?;
    session(db, cfg, tls).await
}

async fn session<S: AsyncRead + AsyncWrite + Unpin>(db: &PgPool, cfg: &ImapConfig, stream: S) -> anyhow::Result<usize> {
    let mut s = Session::new(stream);
    let (greeting, _) = s.read_line().await?;
    if !greeting.starts_with("* OK") && !greeting.starts_with("* PREAUTH") {
        bail!("Unerwartete Begrüßung: {greeting}");
    }
    s.command(&format!("LOGIN {} {}", quote(&cfg.user), quote(&cfg.password))).await?;
    s.command(&format!("SELECT {}", quote(&cfg.folder))).await?;
    let uids: Vec<u32> = s
        .command("UID SEARCH UNSEEN")
        .await?
        .iter()
        .filter_map(|(line, _)| line.strip_prefix("* SEARCH"))
        .flat_map(|rest| rest.split_whitespace().filter_map(|u| u.parse().ok()).collect::<Vec<_>>())
        .take(BATCH)
        .collect();
    let mut imported = 0;
    for uid in uids {
        let raw = s
            .command(&format!("UID FETCH {uid} (BODY.PEEK[])"))
            .await?
            .into_iter()
            .find_map(|(_, mut literals)| literals.pop());
        if let Some(raw) = raw {
            match import(db, &raw).await {
                Ok(true) => imported += 1,
                Ok(false) => {}
                // Nicht verarbeitbare Mails nicht endlos erneut versuchen.
                Err(e) => tracing::warn!(uid, "IMAP-Mail übersprungen: {e:#}"),
            }
        }
        s.command(&format!("UID STORE {uid} +FLAGS.SILENT (\\Seen)")).await?;
    }
    s.command("LOGOUT").await.ok();
    Ok(imported)
}

/// Eine Rohmail importieren. `Ok(false)` = bewusst ignoriert (z. B. Abwesenheitsnotiz).
pub async fn import(db: &PgPool, raw: &[u8]) -> anyhow::Result<bool> {
    let msg = MessageParser::default().parse(raw).ok_or_else(|| anyhow!("E-Mail nicht lesbar"))?;
    // Schleifen vermeiden: automatische Antworten und eigene Mails ignorieren.
    let auto = msg.header_raw("Auto-Submitted").map(|v| v.trim().to_lowercase()).filter(|v| v != "no");
    let bulk = msg.header_raw("Precedence").map(|v| v.trim().to_lowercase()).filter(|v| v == "bulk" || v == "junk" || v == "list");
    if auto.is_some() || bulk.is_some() {
        return Ok(false);
    }
    let sender = msg.from().and_then(|a| a.first()).ok_or_else(|| anyhow!("Kein Absender"))?;
    let address = sender.address().unwrap_or_default().trim().to_lowercase();
    let own = email::smtp_config(db).await.map_err(|e| anyhow!("{e:?}"))?.map(|c| c.from.to_lowercase());
    if own.as_deref() == Some(address.as_str()) {
        return Ok(false);
    }
    let inbound = InboundEmail {
        from: address.clone(),
        from_name: sender.name().unwrap_or_default().to_string(),
        subject: msg.subject().unwrap_or_default().to_string(),
        text: msg.body_text(0).map(|t| t.into_owned()).unwrap_or_default(),
    };
    let ticket_id = match email::handle_inbound(db, inbound).await.map_err(|e| anyhow!(e.public_message()))? {
        InboundResult::Reply(id) | InboundResult::Created(id) => id,
    };

    // Anhänge (ohne eingebettete Bilder aus Signaturen) an das Ticket hängen.
    let files: Vec<Upload> = msg
        .attachments()
        .filter(|part| {
            let inline = part.content_disposition().is_some_and(|d| d.ctype().eq_ignore_ascii_case("inline"));
            !(inline && part.content_id().is_some())
        })
        .map(|part| Upload {
            filename: part.attachment_name().unwrap_or("anhang").to_string(),
            content_type: part
                .content_type()
                .map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")))
                .unwrap_or_else(|| "application/octet-stream".into()),
            data: part.contents().to_vec(),
        })
        .collect();
    if !files.is_empty() {
        let user: Option<User> = sqlx::query_as("SELECT * FROM users WHERE lower(email) = $1").bind(&address).fetch_optional(db).await?;
        if let Some(user) = user {
            if let Err(e) = service::attach_files(db, &user, ticket_id, true, "Anhänge aus der E-Mail", files, "email").await {
                tracing::warn!(ticket = ticket_id, "Anhänge aus E-Mail nicht übernommen: {}", e.public_message());
            }
        }
    }
    Ok(true)
}

/// Hintergrundschleife: liest den Zugang alle paar Sekunden neu (Änderungen in der
/// Verwaltung wirken sofort) und ruft im eingestellten Abstand ab.
pub fn spawn(db: PgPool) {
    tokio::spawn(async move {
        let mut last: Option<ImapConfig> = None;
        let mut next_due = tokio::time::Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let cfg = match current(&db).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("IMAP: Einstellungen nicht lesbar: {}", e.public_message());
                    continue;
                }
            };
            let changed = cfg != last;
            if changed {
                match &cfg {
                    Some(c) => tracing::info!(host = %c.host, folder = %c.folder, "IMAP-Abruf aktiv, alle {} s", c.interval),
                    None if last.is_some() => tracing::info!("IMAP-Abruf beendet"),
                    None => {}
                }
                if cfg.is_none() {
                    clear_status();
                }
                last = cfg.clone();
            }
            let forced = POLL_NOW.swap(false, Ordering::Relaxed);
            let Some(cfg) = cfg else { continue };
            let now = tokio::time::Instant::now();
            if !(changed || forced || now >= next_due) {
                continue;
            }
            next_due = now + Duration::from_secs(cfg.interval);
            match poll(&db, &cfg).await {
                Ok(n) => {
                    if n > 0 {
                        tracing::info!("IMAP: {n} E-Mails übernommen");
                    }
                    set_status(true, format!("{n} neue E-Mails übernommen"));
                }
                Err(e) => {
                    tracing::warn!("IMAP-Abruf fehlgeschlagen: {e:#}");
                    set_status(false, format!("{e:#}"));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_helpers() {
        assert_eq!(literal_len("* 1 FETCH (UID 7 BODY[] {1234}"), Some(1234));
        assert_eq!(literal_len("* 1 FETCH (UID 7 FLAGS (\\Seen))"), None);
        assert_eq!(quote(r#"pa"ss\wort"#), r#""pa\"ss\\wort""#);
    }
}
