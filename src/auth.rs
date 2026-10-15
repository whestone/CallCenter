use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use base64::Engine;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    error::{AppError, AppResult},
    models::User,
    web::WebError,
    AppState,
};

pub const COOKIE: &str = "cc_session";
/// Gültigkeit einer Anmeldung in Tagen (Verwaltung → Einstellungen).
fn session_days() -> i64 {
    crate::config::int("auth.session_days")
}

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
        .map_err(|e| anyhow::anyhow!("salt: {e}"))?;
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hash: {e}"))?
        .to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

pub async fn create_session(db: &PgPool, user_id: i64) -> AppResult<String> {
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO sessions (token, user_id, expires_at) VALUES ($1, $2, now() + make_interval(days => $3))",
    )
    .bind(&token)
    .bind(user_id)
    .bind(session_days() as i32)
    .execute(db)
    .await?;
    Ok(token)
}

/// Löscht die Session (und abgelaufene); gibt den Nutzer der Session zurück.
pub async fn delete_session(db: &PgPool, token: &str) -> AppResult<Option<i64>> {
    let user_id = sqlx::query_scalar("DELETE FROM sessions WHERE token = $1 RETURNING user_id")
        .bind(token)
        .fetch_optional(db)
        .await?;
    sqlx::query("DELETE FROM sessions WHERE expires_at < now()").execute(db).await?;
    Ok(user_id)
}

async fn session_user(db: &PgPool, token: &str) -> AppResult<Option<User>> {
    Ok(sqlx::query_as(
        "SELECT u.* FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token = $1 AND s.expires_at > now() AND u.active",
    )
    .bind(token)
    .fetch_optional(db)
    .await?)
}

/// Prüft E-Mail/Passwort eines aktiven Agenten.
pub async fn authenticate(db: &PgPool, email: &str, password: &str) -> AppResult<Option<User>> {
    let user: Option<User> =
        sqlx::query_as("SELECT * FROM users WHERE lower(email) = lower($1) AND active")
            .bind(email.trim())
            .fetch_optional(db)
            .await?;
    Ok(user.filter(|u| {
        u.is_agent() && u.password_hash.as_deref().is_some_and(|h| verify_password(password, h))
    }))
}

/// Prüft E-Mail/Passwort eines beliebigen aktiven Nutzers (Kundenportal).
pub async fn authenticate_any(db: &PgPool, email: &str, password: &str) -> AppResult<Option<User>> {
    let user: Option<User> =
        sqlx::query_as("SELECT * FROM users WHERE lower(email) = lower($1) AND active")
            .bind(email.trim())
            .fetch_optional(db)
            .await?;
    Ok(user.filter(|u| u.password_hash.as_deref().is_some_and(|h| verify_password(password, h))))
}

/// Angemeldeter Nutzer im Kundenportal (jede Rolle); sonst Weiterleitung zum Portal-Login.
pub struct PortalUser(pub User);

impl FromRequestParts<AppState> for PortalUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let user = match cookie(&parts.headers, COOKIE) {
            Some(token) => session_user(&state.db, &token).await.ok().flatten(),
            None => None,
        };
        user.map(PortalUser).ok_or_else(|| Redirect::to("/portal/login").into_response())
    }
}

/// Optional angemeldeter Nutzer (für öffentliche Portalseiten wie das Hilfe-Center).
pub struct MaybePortalUser(pub Option<User>);

impl FromRequestParts<AppState> for MaybePortalUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let user = match cookie(&parts.headers, COOKIE) {
            Some(token) => session_user(&state.db, &token).await.ok().flatten(),
            None => None,
        };
        Ok(MaybePortalUser(user))
    }
}

pub fn session_cookie(token: &str) -> String {
    format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}", session_days() * 86400)
}

pub fn clear_cookie() -> String {
    format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(';'))
        .find_map(|pair| {
            let (k, v) = pair.trim().split_once('=')?;
            (k == name).then(|| v.to_string())
        })
}

async fn cookie_agent(parts: &Parts, state: &AppState) -> Option<User> {
    let token = cookie(&parts.headers, COOKIE)?;
    session_user(&state.db, &token).await.ok().flatten().filter(User::is_agent)
}

fn login_redirect(headers: &HeaderMap) -> Response {
    if headers.contains_key("hx-request") {
        (StatusCode::OK, [("HX-Redirect", "/login")]).into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}

/// Angemeldeter Agent (Web-Oberfläche, Session-Cookie).
pub struct WebAgent(pub User);

impl FromRequestParts<AppState> for WebAgent {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        match cookie_agent(parts, state).await {
            Some(u) => Ok(WebAgent(u)),
            None => Err(login_redirect(&parts.headers)),
        }
    }
}

/// Angemeldeter Administrator (Web-Oberfläche).
pub struct WebAdmin(pub User);

impl FromRequestParts<AppState> for WebAdmin {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let WebAgent(user) = WebAgent::from_request_parts(parts, state).await?;
        if user.is_admin() {
            Ok(WebAdmin(user))
        } else {
            Err(WebError(AppError::Forbidden).into_response())
        }
    }
}

// ---------------------------------------------------------------------------
// API-Tokens
// ---------------------------------------------------------------------------

pub fn token_hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(token.trim().as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Erzeugt einen neuen Token; der Klartext wird nur einmal zurückgegeben.
pub async fn create_api_token(db: &PgPool, user_id: i64, name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::invalid("Bitte einen Namen für den Token angeben, z. B. „Telefonanlage“"));
    }
    let token = format!("cc_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    sqlx::query("INSERT INTO api_tokens (user_id, name, token_hash) VALUES ($1, $2, $3)")
        .bind(user_id)
        .bind(name)
        .bind(token_hash(&token))
        .execute(db)
        .await?;
    Ok(token)
}

async fn token_user(db: &PgPool, token: &str) -> AppResult<Option<User>> {
    let user: Option<User> = sqlx::query_as(
        "UPDATE api_tokens t SET last_used_at = now() FROM users u
         WHERE t.token_hash = $1 AND u.id = t.user_id AND u.active
         RETURNING u.*",
    )
    .bind(token_hash(token))
    .fetch_optional(db)
    .await?;
    Ok(user.filter(User::is_agent))
}

/// API-Nutzer (nur Agenten). Unterstützt:
/// - `Authorization: Bearer <token>`
/// - HTTP Basic `E-Mail/token:<token>` (wie Zendesk) oder `E-Mail:Passwort`
/// - Session-Cookie der Web-Oberfläche
pub struct ApiAgent(pub User);

impl FromRequestParts<AppState> for ApiAgent {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let Some(auth) = parts.headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
            return cookie_agent(parts, state).await.map(ApiAgent).ok_or(AppError::Unauthorized);
        };
        if let Some(token) = auth.strip_prefix("Bearer ") {
            return token_user(&state.db, token).await?.map(ApiAgent).ok_or(AppError::Unauthorized);
        }
        let encoded = auth.strip_prefix("Basic ").ok_or(AppError::Unauthorized)?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|_| AppError::Unauthorized)?;
        let decoded = String::from_utf8(decoded).map_err(|_| AppError::Unauthorized)?;
        let (login, secret) = decoded.split_once(':').ok_or(AppError::Unauthorized)?;
        let user = match login.strip_suffix("/token") {
            Some(email) => token_user(&state.db, secret)
                .await?
                .filter(|u| u.email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(email.trim()))),
            // Konten mit 2FA dürfen die API nur mit Token nutzen, nicht mit dem Passwort.
            None => authenticate(&state.db, login, secret).await?.filter(|u| u.totp_secret.is_none()),
        };
        user.map(ApiAgent).ok_or(AppError::Unauthorized)
    }
}
