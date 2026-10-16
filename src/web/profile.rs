//! Persönliche Profilseite: Profilfoto, Passwort ändern, API-Tokens verwalten.

use askama::Template;
use axum::{
    extract::{DefaultBodyLimit, Multipart},
    http::header,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::extract::{Form, Path, State};
use super::*;
use crate::{
    auth::{self, WebAgent},
    service::{self, UserInput},
    twofactor,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(profile))
        .route("/me/password", post(change_password))
        .route("/me/tokens", post(create_token))
        .route("/me/tokens/{id}/delete", post(delete_token))
        .route("/me/2fa/setup", post(twofa_setup))
        .route("/me/2fa/enable", post(twofa_enable))
        .route("/me/2fa/cancel", post(twofa_cancel))
        .route("/me/2fa/disable", post(twofa_disable))
        .route("/me/avatar", post(upload_avatar).layer(DefaultBodyLimit::max(crate::avatars::MAX_BYTES + 256 * 1024)))
        .route("/me/avatar/delete", post(delete_avatar))
        .route("/avatars/{id}", get(avatar_image))
}

// ---------------------------------------------------------------------------
// Profilfoto
// ---------------------------------------------------------------------------

async fn upload_avatar(State(st): State<AppState>, WebAgent(me): WebAgent, mut multipart: Multipart) -> WebResult<Response> {
    let mut data: Option<Vec<u8>> = None;
    let mut fail = None;
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) if field.name() == Some("photo") => match field.bytes().await {
                Ok(b) => data = Some(b.to_vec()),
                Err(_) => fail = Some("Das Bild ist größer als 2 MB"),
            },
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                fail = Some("Das Bild ist größer als 2 MB");
                break;
            }
        }
    }
    let result = match (fail, data) {
        (Some(msg), _) => Err(AppError::invalid(msg)),
        (None, data) => crate::avatars::save(&st.db, me.id, &data.unwrap_or_default()).await,
    };
    match result {
        Ok(()) => Ok(see_other_ok("/me", "Profilfoto gespeichert")),
        Err(AppError::Unprocessable(msg)) => {
            let html = render(&st.db, &me, String::new(), String::new(), msg).await?;
            Ok((axum::http::StatusCode::UNPROCESSABLE_ENTITY, html).into_response())
        }
        Err(e) => Err(e.into()),
    }
}

async fn delete_avatar(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Response> {
    crate::avatars::remove(&st.db, me.id).await?;
    Ok(see_other_ok("/me", "Profilfoto entfernt"))
}

/// Foto ausliefern (nur für angemeldete Mitarbeitende). Die Adresse enthält die
/// Version, deshalb darf der Browser lange zwischenspeichern.
async fn avatar_image(State(st): State<AppState>, WebAgent(_): WebAgent, Path(id): Path<i64>) -> WebResult<Response> {
    let Some((content_type, data)) = crate::avatars::load(&st.db, id).await? else {
        return Err(AppError::NotFound.into());
    };
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "private, max-age=31536000, immutable".to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (header::CONTENT_SECURITY_POLICY, "default-src 'none'".to_string()),
        ],
        data,
    )
        .into_response())
}

struct TokenRow {
    id: i64,
    name: String,
    created: String,
    last_used: String,
}

#[derive(Template)]
#[template(path = "profile.html")]
struct ProfilePage {
    nav: Nav,
    name: String,
    email: String,
    role_label: &'static str,
    user_id: i64,
    initials: String,
    has_photo: bool,
    tokens: Vec<TokenRow>,
    new_token: String,
    notice: String,
    error: String,
    twofa_enabled: bool,
    setup_qr: String,
    setup_secret: String,
}

async fn render(db: &PgPool, me: &User, new_token: String, notice: String, error: String) -> WebResult<Html<String>> {
    let rows: Vec<(i64, String, DateTime<Utc>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT id, name, created_at, last_used_at FROM api_tokens WHERE user_id = $1 ORDER BY created_at DESC",
    )
    .bind(me.id)
    .fetch_all(db)
    .await?;
    let now = Utc::now();
    page(ProfilePage {
        nav: Nav::new(me, "me"),
        name: me.name.clone(),
        email: me.email.clone().unwrap_or_default(),
        role_label: role_label(&me.role),
        user_id: me.id,
        initials: initials(&me.name),
        has_photo: crate::avatars::url(me.id).is_some(),
        tokens: rows
            .into_iter()
            .map(|(id, name, created, used)| TokenRow {
                id,
                name,
                created: fmt_dt(created),
                last_used: used.map(|u| fmt_rel(u, now)).unwrap_or_else(|| "nie".into()),
            })
            .collect(),
        new_token,
        notice,
        error,
        twofa_enabled: me.totp_secret.is_some(),
        setup_qr: me
            .totp_pending_secret
            .as_deref()
            .map(|s| twofactor::qr_svg(&twofactor::otpauth_url(s, me.email.as_deref().unwrap_or_default())))
            .unwrap_or_default(),
        setup_secret: me.totp_pending_secret.clone().unwrap_or_default(),
    })
}

async fn reload(db: &PgPool, id: i64) -> WebResult<User> {
    Ok(sqlx::query_as("SELECT * FROM users WHERE id = $1").bind(id).fetch_one(db).await?)
}

async fn twofa_setup(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Html<String>> {
    sqlx::query("UPDATE users SET totp_pending_secret = $2 WHERE id = $1")
        .bind(me.id)
        .bind(twofactor::new_secret())
        .execute(&st.db)
        .await?;
    let me = reload(&st.db, me.id).await?;
    render(&st.db, &me, String::new(), String::new(), String::new()).await
}

#[derive(Deserialize)]
struct CodeForm {
    code: String,
}

async fn twofa_enable(State(st): State<AppState>, WebAgent(me): WebAgent, Form(f): Form<CodeForm>) -> WebResult<Html<String>> {
    let account = me.email.clone().unwrap_or_default();
    let ok = me.totp_pending_secret.as_deref().is_some_and(|s| twofactor::verify(s, &account, &f.code));
    if !ok {
        return render(&st.db, &me, String::new(), String::new(), "Der Code stimmt nicht. Bitte den aktuellen Code aus der App eingeben.".into()).await;
    }
    sqlx::query("UPDATE users SET totp_secret = totp_pending_secret, totp_pending_secret = NULL WHERE id = $1")
        .bind(me.id)
        .execute(&st.db)
        .await?;
    let me = reload(&st.db, me.id).await?;
    render(&st.db, &me, String::new(), "Zwei-Faktor-Anmeldung ist aktiv. Ab der nächsten Anmeldung wird der Code abgefragt.".into(), String::new()).await
}

async fn twofa_cancel(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Response> {
    sqlx::query("UPDATE users SET totp_pending_secret = NULL WHERE id = $1").bind(me.id).execute(&st.db).await?;
    Ok(see_other("/me"))
}

#[derive(Deserialize)]
struct DisableForm {
    password: String,
}

async fn twofa_disable(State(st): State<AppState>, WebAgent(me): WebAgent, Form(f): Form<DisableForm>) -> WebResult<Html<String>> {
    if !me.password_hash.as_deref().is_some_and(|h| auth::verify_password(&f.password, h)) {
        return render(&st.db, &me, String::new(), String::new(), "Das Passwort ist falsch.".into()).await;
    }
    sqlx::query("UPDATE users SET totp_secret = NULL WHERE id = $1").bind(me.id).execute(&st.db).await?;
    let me = reload(&st.db, me.id).await?;
    render(&st.db, &me, String::new(), "Zwei-Faktor-Anmeldung wurde ausgeschaltet.".into(), String::new()).await
}

async fn profile(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Html<String>> {
    render(&st.db, &me, String::new(), String::new(), String::new()).await
}

#[derive(Deserialize)]
struct PasswordForm {
    current: String,
    new_password: String,
    repeat: String,
}

async fn change_password(State(st): State<AppState>, WebAgent(me): WebAgent, Form(f): Form<PasswordForm>) -> WebResult<Response> {
    let current_ok = me.password_hash.as_deref().is_some_and(|h| auth::verify_password(&f.current, h));
    let error = if !current_ok {
        "Das aktuelle Passwort ist falsch.".to_string()
    } else if f.new_password != f.repeat {
        "Die neuen Passwörter stimmen nicht überein.".to_string()
    } else {
        let input = UserInput { password: Some(f.new_password.clone()), ..Default::default() };
        match service::update_user(&st.db, &me, me.id, input).await {
            Ok(updated) => {
                let html = render(&st.db, &updated, String::new(), "Passwort geändert.".into(), String::new()).await?;
                return Ok(html.into_response());
            }
            Err(AppError::Unprocessable(msg)) => msg,
            Err(e) => return Err(e.into()),
        }
    };
    let html = render(&st.db, &me, String::new(), String::new(), error).await?;
    Ok((axum::http::StatusCode::UNPROCESSABLE_ENTITY, html).into_response())
}

#[derive(Deserialize)]
struct TokenForm {
    name: String,
}

async fn create_token(State(st): State<AppState>, WebAgent(me): WebAgent, Form(f): Form<TokenForm>) -> WebResult<Html<String>> {
    match auth::create_api_token(&st.db, me.id, &f.name).await {
        Ok(token) => render(&st.db, &me, token, String::new(), String::new()).await,
        Err(AppError::Unprocessable(msg)) => render(&st.db, &me, String::new(), String::new(), msg).await,
        Err(e) => Err(e.into()),
    }
}

async fn delete_token(State(st): State<AppState>, WebAgent(me): WebAgent, Path(id): Path<i64>) -> WebResult<Response> {
    // Nur eigene Tokens widerrufen.
    sqlx::query("DELETE FROM api_tokens WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(me.id)
        .execute(&st.db)
        .await?;
    Ok(see_other("/me"))
}
