//! Zwei-Faktor-Anmeldung per TOTP (RFC 6238, kompatibel mit gängigen Authenticator-Apps).

use qrcodegen::{QrCode, QrCodeEcc};
use sqlx::PgPool;
use totp_rs::{Algorithm, Secret, TOTP};
use uuid::Uuid;

use crate::{auth, error::AppResult, models::User};



fn totp(secret: &str, account: &str) -> Option<TOTP> {
    let bytes = Secret::Encoded(secret.to_string()).to_bytes().ok()?;
    // ':' ist im Kontonamen nicht erlaubt.
    TOTP::new(Algorithm::SHA1, 6, 1, 30, bytes, Some(crate::config::brand().replace(':', "")), account.replace(':', "")).ok()
}

pub fn new_secret() -> String {
    Secret::generate_secret().to_encoded().to_string()
}

/// Prüft einen 6-stelligen Code (±30 Sekunden Toleranz).
pub fn verify(secret: &str, account: &str, code: &str) -> bool {
    let code: String = code.chars().filter(char::is_ascii_digit).collect();
    code.len() == 6 && totp(secret, account).is_some_and(|t| t.check_current(&code).unwrap_or(false))
}

pub fn otpauth_url(secret: &str, account: &str) -> String {
    totp(secret, account).map(|t| t.get_url()).unwrap_or_default()
}

/// QR-Code als SVG (wird lokal erzeugt, das Secret verlässt den Server nicht).
pub fn qr_svg(text: &str) -> String {
    let Ok(qr) = QrCode::encode_text(text, QrCodeEcc::Medium) else { return String::new() };
    let border = 4;
    let size = qr.size() + border * 2;
    let mut path = String::new();
    for y in 0..qr.size() {
        for x in 0..qr.size() {
            if qr.get_module(x, y) {
                path.push_str(&format!("M{},{}h1v1h-1z", x + border, y + border));
            }
        }
    }
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" width="200" height="200" shape-rendering="crispEdges" role="img" aria-label="QR-Code für die Authenticator-App"><rect width="100%" height="100%" fill="#fff"/><path d="{path}" fill="#000"/></svg>"##
    )
}

// ---------------------------------------------------------------------------
// Login-Zwischenschritt
// ---------------------------------------------------------------------------

pub async fn create_challenge(db: &PgPool, user_id: i64) -> AppResult<String> {
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    sqlx::query("DELETE FROM login_challenges WHERE expires_at < now()").execute(db).await?;
    sqlx::query("INSERT INTO login_challenges (token_hash, user_id, expires_at) VALUES ($1, $2, now() + interval '5 minutes')")
        .bind(auth::token_hash(&token))
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(token)
}

pub enum ChallengeResult {
    Ok(User),
    WrongCode,
    Expired,
}

/// Prüft den Code zu einem Login-Zwischenschritt; max. 5 Versuche.
pub async fn complete_challenge(db: &PgPool, token: &str, code: &str) -> AppResult<ChallengeResult> {
    let hash = auth::token_hash(token);
    let user: Option<User> = sqlx::query_as(
        "SELECT u.* FROM login_challenges c JOIN users u ON u.id = c.user_id
         WHERE c.token_hash = $1 AND c.expires_at > now() AND c.attempts < 5 AND u.active",
    )
    .bind(&hash)
    .fetch_optional(db)
    .await?;
    let Some(user) = user else { return Ok(ChallengeResult::Expired) };
    let account = user.email.clone().unwrap_or_default();
    if user.totp_secret.as_deref().is_some_and(|s| verify(s, &account, code)) {
        sqlx::query("DELETE FROM login_challenges WHERE token_hash = $1").bind(&hash).execute(db).await?;
        return Ok(ChallengeResult::Ok(user));
    }
    sqlx::query("UPDATE login_challenges SET attempts = attempts + 1 WHERE token_hash = $1").bind(&hash).execute(db).await?;
    Ok(ChallengeResult::WrongCode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let secret = new_secret();
        let t = totp(&secret, "anna@example.com").unwrap();
        let code = t.generate_current().unwrap();
        assert!(verify(&secret, "anna@example.com", &code));
        assert!(verify(&secret, "anna@example.com", &format!("{} {}", &code[..3], &code[3..])));
        assert!(!verify(&secret, "anna@example.com", "000000") || code == "000000");
        assert!(otpauth_url(&secret, "anna@example.com").starts_with("otpauth://totp/CallCenter:"));
        assert!(qr_svg("hallo").starts_with("<svg"));
    }
}
