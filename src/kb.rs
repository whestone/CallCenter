//! Wissensdatenbank: Artikel, Kategorien, Volltextsuche (deutsche Wortstämme),
//! Vorschläge und Bewertungen.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Article {
    pub id: i64,
    pub category_id: Option<i64>,
    pub title: String,
    pub body: String,
    pub status: String,
    pub author_id: Option<i64>,
    pub views: i32,
    pub helpful_yes: i32,
    pub helpful_no: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLUMNS: &str =
    "id, category_id, title, body, status, author_id, views, helpful_yes, helpful_no, created_at, updated_at";

pub async fn categories(db: &PgPool) -> AppResult<Vec<(i64, String)>> {
    Ok(sqlx::query_as("SELECT id, name FROM kb_categories ORDER BY position, name").fetch_all(db).await?)
}

pub async fn create_category(db: &PgPool, name: &str) -> AppResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::invalid("Bitte einen Namen angeben"));
    }
    sqlx::query("INSERT INTO kb_categories (name) VALUES ($1)").bind(name).execute(db).await?;
    Ok(())
}

/// Suche über Titel und Text. Leere Suche liefert die neuesten Artikel.
/// `any_word`: Treffer, wenn mindestens ein Wort passt (für Vorschläge).
pub async fn search(db: &PgPool, query: &str, only_published: bool, any_word: bool, limit: i64) -> AppResult<Vec<Article>> {
    let words: Vec<&str> = query.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3).collect();
    if words.is_empty() {
        return Ok(sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM kb_articles WHERE status = 'published' OR NOT $1 ORDER BY updated_at DESC LIMIT $2"
        ))
        .bind(only_published)
        .bind(limit)
        .fetch_all(db)
        .await?);
    }
    let ts = if any_word { words.join(" or ") } else { words.join(" ") };
    Ok(sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM kb_articles
         WHERE (search @@ websearch_to_tsquery('german', $1) OR title ILIKE $2)
           AND (status = 'published' OR NOT $3)
         ORDER BY ts_rank(search, websearch_to_tsquery('german', $1)) DESC, updated_at DESC
         LIMIT $4"
    ))
    .bind(ts)
    .bind(format!("%{}%", query.trim().replace('%', "").replace('_', "")))
    .bind(only_published)
    .bind(limit)
    .fetch_all(db)
    .await?)
}

pub async fn get(db: &PgPool, id: i64) -> AppResult<Article> {
    sqlx::query_as(&format!("SELECT {COLUMNS} FROM kb_articles WHERE id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

pub struct ArticleInput {
    pub title: String,
    pub body: String,
    pub category_id: Option<i64>,
    pub status: String,
}

pub async fn save(db: &PgPool, id: Option<i64>, input: ArticleInput, author_id: i64) -> AppResult<i64> {
    let title = input.title.trim();
    if title.is_empty() {
        return Err(AppError::invalid("Der Titel darf nicht leer sein"));
    }
    if !matches!(input.status.as_str(), "draft" | "published") {
        return Err(AppError::invalid("Ungültiger Status"));
    }
    let body = input.body.replace("\r\n", "\n");
    Ok(match id {
        Some(id) => sqlx::query_scalar(
            "UPDATE kb_articles SET title = $2, body = $3, category_id = $4, status = $5, updated_at = now()
             WHERE id = $1 RETURNING id",
        )
        .bind(id)
        .bind(title)
        .bind(body)
        .bind(input.category_id)
        .bind(&input.status)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)?,
        None => sqlx::query_scalar(
            "INSERT INTO kb_articles (title, body, category_id, status, author_id) VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(title)
        .bind(body)
        .bind(input.category_id)
        .bind(&input.status)
        .bind(author_id)
        .fetch_one(db)
        .await?,
    })
}

pub async fn count_view(db: &PgPool, id: i64) -> AppResult<()> {
    sqlx::query("UPDATE kb_articles SET views = views + 1 WHERE id = $1").bind(id).execute(db).await?;
    Ok(())
}

pub async fn vote(db: &PgPool, id: i64, helpful: bool) -> AppResult<()> {
    let column = if helpful { "helpful_yes" } else { "helpful_no" };
    sqlx::query(&format!("UPDATE kb_articles SET {column} = {column} + 1 WHERE id = $1 AND status = 'published'"))
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Kurzer Auszug für Listen und Vorschläge.
pub fn excerpt(body: &str, chars: usize) -> String {
    let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= chars {
        flat
    } else {
        format!("{} …", flat.chars().take(chars).collect::<String>().trim_end())
    }
}

pub fn article_url(id: i64) -> String {
    format!("{}/portal/help/{id}", crate::email::public_url())
}
