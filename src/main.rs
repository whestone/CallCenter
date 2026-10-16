mod api;
mod auth;
mod automations;
mod automode;
mod avatars;
mod chat;
mod config;
mod email;
mod error;
mod escalation;
mod imap;
mod kb;
mod markdown;
mod models;
mod notify;
mod presence;
mod routing;
mod rules;
mod search;
mod security;
mod seed;
mod service;
mod sla;
mod statuses;
mod telephony;
mod twofactor;
mod web;
mod webhooks;

use std::net::SocketAddr;

use axum::http::{header, HeaderValue};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceBuilder;
use tower_http::{services::ServeDir, set_header::SetResponseHeaderLayer, trace::TraceLayer};

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub presence: std::sync::Arc<presence::Presence>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "callcenter=info,tower_http=info".into()),
        )
        .init();

    let url = std::env::var("DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("DATABASE_URL ist nicht gesetzt (siehe .env.example)"))?;
    // Direkt nach dem Hochfahren braucht die Datenbank oft noch ein paar Sekunden
    // (z. B. Wiederherstellung nach einem unsauberen Ende) – so lange warten.
    let mut attempt = 0;
    let db = loop {
        match PgPoolOptions::new().max_connections(10).connect(&url).await {
            Ok(db) => break db,
            Err(e) if attempt < 30 => {
                if attempt == 0 {
                    tracing::warn!("Datenbank noch nicht erreichbar ({e}) – warte bis zu 30 Sekunden …");
                }
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Datenbank nicht erreichbar: {e}\nLäuft PostgreSQL? Stimmt DATABASE_URL in der .env?"
                ))
            }
        }
    };
    sqlx::migrate!().run(&db).await?;
    seed::run(&db).await?;
    config::reload(&db).await?;
    statuses::reload(&db).await?;
    avatars::reload(&db).await?;
    automode::reload(&db).await?;

    let state = AppState { db, presence: Default::default() };
    automations::spawn(state.clone());
    email::spawn_sender(state.db.clone());
    imap::spawn(state.db.clone());
    webhooks::spawn(state.db.clone());

    let app = axum::Router::new()
        .merge(web::router())
        .nest("/api/v2", api::router())
        // no-cache: Browser fragt bei jedem Aufruf nach (304 bei unveränderter Datei),
        // damit nach einem Update kein veraltetes CSS/JS hängen bleibt.
        .nest_service(
            "/static",
            ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::overriding(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("no-cache"),
                ))
                .service(ServeDir::new("static")),
        )
        .layer(axum::middleware::from_fn(security::csrf_guard))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3000".into())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        anyhow::anyhow!("Port {addr} ist nicht verfügbar ({e}).\nLäuft CallCenter schon in einem anderen Fenster? Sonst BIND_ADDR in der .env ändern.")
    })?;
    tracing::info!("CallCenter läuft auf http://{addr} (auch http://localhost:{})", addr.port());

    // "localhost" wird unter Windows zuerst als IPv6 (::1) aufgelöst. Bei lokaler
    // IPv4-Bindung zusätzlich auf ::1 lauschen, sonst laden manche Browser nicht.
    if addr.ip() == std::net::Ipv4Addr::LOCALHOST {
        let v6 = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, addr.port()));
        match tokio::net::TcpListener::bind(v6).await {
            Ok(listener_v6) => {
                let app_v6 = app.clone();
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener_v6, app_v6).await {
                        tracing::warn!("IPv6-Listener beendet: {e}");
                    }
                });
            }
            Err(e) => tracing::warn!("IPv6 (::1) nicht verfügbar: {e}"),
        }
    }
    axum::serve(listener, app).await?;
    Ok(())
}
