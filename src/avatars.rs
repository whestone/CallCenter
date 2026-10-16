//! Profilfotos: Speichern, Ausliefern und ein Zwischenspeicher, welche Nutzer ein
//! Foto haben (damit Vorlagen ohne Datenbankzugriff entscheiden können, ob ein Bild
//! oder die Initialen erscheinen).

use std::{borrow::Borrow, collections::HashMap, sync::RwLock};

use sqlx::PgPool;

use crate::error::{AppError, AppResult};

/// Höchstgröße eines Fotos.
pub const MAX_BYTES: usize = 2 * 1024 * 1024;

/// Nutzer-ID → Version (Zeitstempel), Teil der Bildadresse für sauberes Caching.
static VERSIONS: RwLock<Option<HashMap<i64, i64>>> = RwLock::new(None);

pub async fn reload(db: &PgPool) -> AppResult<()> {
    let rows: Vec<(i64, i64)> =
        sqlx::query_as("SELECT user_id, (extract(epoch FROM updated_at) * 1000)::bigint FROM user_avatars").fetch_all(db).await?;
    if let Ok(mut v) = VERSIONS.write() {
        *v = Some(rows.into_iter().collect());
    }
    Ok(())
}

/// Adresse des Fotos oder `None`, wenn der Nutzer keins hat.
/// Nimmt `i64` oder `&i64` (Vorlagen reichen Referenzen weiter).
pub fn url(user_id: impl Borrow<i64>) -> Option<String> {
    let user_id = *user_id.borrow();
    let version = *VERSIONS.read().ok()?.as_ref()?.get(&user_id)?;
    Some(format!("/avatars/{user_id}?v={version}"))
}

/// Adresse für eine ID als Text (z. B. Wert einer Auswahl); leer, wenn kein Foto.
pub fn url_for(id: &str) -> String {
    id.parse::<i64>().ok().and_then(url).unwrap_or_default()
}

/// Bildformat anhand der ersten Bytes erkennen (keine SVGs – die könnten Skripte enthalten).
fn sniff(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.len() > 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

pub async fn save(db: &PgPool, user_id: i64, data: &[u8]) -> AppResult<()> {
    if data.is_empty() {
        return Err(AppError::invalid("Bitte ein Bild auswählen"));
    }
    if data.len() > MAX_BYTES {
        return Err(AppError::invalid("Das Bild ist größer als 2 MB"));
    }
    let Some(content_type) = sniff(data) else {
        return Err(AppError::invalid("Bitte ein Bild im Format PNG, JPEG, WebP oder GIF hochladen"));
    };
    sqlx::query(
        "INSERT INTO user_avatars (user_id, content_type, data, updated_at) VALUES ($1, $2, $3, now())
         ON CONFLICT (user_id) DO UPDATE SET content_type = $2, data = $3, updated_at = now()",
    )
    .bind(user_id)
    .bind(content_type)
    .bind(data)
    .execute(db)
    .await?;
    reload(db).await
}

pub async fn remove(db: &PgPool, user_id: i64) -> AppResult<()> {
    sqlx::query("DELETE FROM user_avatars WHERE user_id = $1").bind(user_id).execute(db).await?;
    reload(db).await
}

pub async fn load(db: &PgPool, user_id: i64) -> AppResult<Option<(String, Vec<u8>)>> {
    Ok(sqlx::query_as("SELECT content_type, data FROM user_avatars WHERE user_id = $1").bind(user_id).fetch_optional(db).await?)
}

/// HTML für einen Avatar: Foto, falls vorhanden, sonst die Initialen.
/// `class` sind zusätzliche Klassen (z. B. "xs", "sm").
pub fn html(user_id: impl Borrow<i64>, initials: &str, class: &str) -> String {
    let class = if class.is_empty() { "avatar".to_string() } else { format!("avatar {class}") };
    match url(user_id) {
        Some(src) => format!(r#"<span class="{class} has-photo" aria-hidden="true"><img src="{src}" alt="" loading="lazy"></span>"#),
        None => format!(r#"<span class="{class}" aria-hidden="true">{}</span>"#, crate::markdown::escape(initials)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffing() {
        assert_eq!(sniff(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]), Some("image/png"));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\">"), None);
    }
}
