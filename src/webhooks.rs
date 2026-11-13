//! Ausgehende Webhooks: Ereignisse werden in der Transaktion des Ticket-Updates
//! vorgemerkt und im Hintergrund zugestellt (HMAC-SHA256-Signatur, Wiederholung
//! mit wachsendem Abstand, max. 5 Versuche).

use std::time::Duration;

use chrono::Utc;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::{PgConnection, PgPool};

use crate::{error::AppResult, models::Ticket};

pub const EVENTS: [(&str, &str); 3] = [
    ("ticket.created", "Ticket erstellt"),
    ("ticket.updated", "Ticket geändert"),
    ("ticket.solved", "Ticket gelöst"),
];

const MAX_ATTEMPTS: i32 = 5;

/// Ereignis für alle aktiven Webhooks vormerken, die es abonniert haben.
pub async fn enqueue(conn: &mut PgConnection, event: &str, ticket: &Ticket) -> AppResult<()> {
    let payload = json!({ "event": event, "occurred_at": Utc::now(), "ticket": ticket });
    sqlx::query(
        "INSERT INTO webhook_deliveries (webhook_id, event, payload)
         SELECT id, $1, $2 FROM webhooks WHERE active AND $1 = ANY(events)",
    )
    .bind(event)
    .bind(payload)
    .execute(conn)
    .await?;
    Ok(())
}

/// Test-Ereignis an einen einzelnen Webhook.
pub async fn ping(db: &PgPool, webhook_id: i64) -> AppResult<()> {
    sqlx::query("INSERT INTO webhook_deliveries (webhook_id, event, payload) VALUES ($1, 'ping', $2)")
        .bind(webhook_id)
        .bind(json!({ "event": "ping", "occurred_at": Utc::now(), "message": "Test vom CallCenter" }))
        .execute(db)
        .await?;
    Ok(())
}

/// Signatur `sha256=<hex>` über den exakten Request-Body.
pub fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC akzeptiert jede Schlüssellänge");
    mac.update(body);
    let hex: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

pub async fn deliver_pending(db: &PgPool, client: &reqwest::Client) -> AppResult<usize> {
    if crate::config::demo_mode() {
        return Ok(0);
    }
    let pending: Vec<(i64, String, Value, i32, String, String)> = sqlx::query_as(
        "SELECT d.id, d.event, d.payload, d.attempts, w.url, w.secret
         FROM webhook_deliveries d JOIN webhooks w ON w.id = d.webhook_id
         WHERE d.status = 'pending' AND d.next_try_at <= now()
         ORDER BY d.id LIMIT 50",
    )
    .fetch_all(db)
    .await?;
    let mut delivered = 0;
    for (id, event, payload, attempts, url, secret) in pending {
        let body = payload.to_string();
        let result = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("X-CallCenter-Event", &event)
            .header("X-CallCenter-Delivery", id.to_string())
            .header("X-CallCenter-Signature", sign(&secret, body.as_bytes()))
            .body(body)
            .send()
            .await;
        let (code, error) = match result {
            Ok(resp) if resp.status().is_success() => (Some(resp.status().as_u16() as i32), None),
            Ok(resp) => (Some(resp.status().as_u16() as i32), Some(format!("HTTP {}", resp.status()))),
            Err(e) => (None, Some(e.to_string())),
        };
        match error {
            None => {
                delivered += 1;
                sqlx::query("UPDATE webhook_deliveries SET status = 'sent', response_code = $2, error = NULL, attempts = attempts + 1, sent_at = now() WHERE id = $1")
                    .bind(id)
                    .bind(code)
                    .execute(db)
                    .await?;
            }
            Some(err) => {
                let attempts = attempts + 1;
                let status = if attempts >= MAX_ATTEMPTS { "failed" } else { "pending" };
                // 1, 2, 4, 8 Minuten Abstand
                let backoff = 1i32 << (attempts - 1).min(4);
                sqlx::query(
                    "UPDATE webhook_deliveries SET status = $2, response_code = $3, error = $4, attempts = $5,
                        next_try_at = now() + make_interval(mins => $6) WHERE id = $1",
                )
                .bind(id)
                .bind(status)
                .bind(code)
                .bind(err)
                .bind(attempts)
                .bind(backoff)
                .execute(db)
                .await?;
            }
        }
    }
    Ok(delivered)
}

pub fn spawn(db: PgPool) {
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("CallCenter-Webhooks/1.0")
            .build()
            .expect("HTTP-Client");
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            if let Err(e) = deliver_pending(&db, &client).await {
                tracing::error!("Webhook-Zustellung: {}", e.public_message());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn signature_is_stable() {
        // Referenzwert aus Python: hmac.new(b"geheim", b'{"a":1}', hashlib.sha256).hexdigest()
        assert_eq!(
            super::sign("geheim", br#"{"a":1}"#),
            "sha256=0c3e89010807a274fd26fae3628d3e1dc8f1ee15cc206a8e72920e50b4da8baf"
        );
        assert_ne!(super::sign("geheim", b"x"), super::sign("anders", b"x"));
    }
}
