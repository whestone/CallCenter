//! Auto-Modus in der Oberfläche: ein-/ausschalten, nächste Aufgabe öffnen, überspringen.

use askama::Template;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde::Deserialize;

use super::extract::{Form, Path, Query, State};
use super::*;
use crate::{auth::WebAgent, automode};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me/auto", post(toggle))
        .route("/auto/next", get(next))
        .route("/auto/skip/{id}", post(skip))
}

#[derive(Deserialize)]
struct ToggleForm {
    on: Option<String>,
}

/// Ein: sofort zur nächsten Aufgabe. Aus: zurück zur Ticketliste.
async fn toggle(State(st): State<AppState>, WebAgent(me): WebAgent, Form(f): Form<ToggleForm>) -> WebResult<Response> {
    let on = f.on.as_deref() == Some("1");
    automode::set_enabled(&st.db, me.id, on).await?;
    Ok(if on { see_other("/auto/next") } else { see_other_ok("/views", "Auto-Modus beendet") })
}

#[derive(Template)]
#[template(path = "auto_wait.html")]
struct WaitPage {
    nav: Nav,
    sources: Vec<String>,
}

#[derive(Deserialize)]
struct NextQuery {
    poll: Option<String>,
    /// Bestätigung vom vorigen Schritt („#12 gelöst“), wird an das nächste Ticket weitergereicht.
    ok: Option<String>,
}

/// Nächste Aufgabe öffnen. Gibt es keine, erscheint eine Warteseite, die regelmäßig
/// nachfragt (`?poll=1` → 204 ohne Treffer, sonst Weiterleitung per HX-Redirect).
async fn next(State(st): State<AppState>, WebAgent(me): WebAgent, Query(q): Query<NextQuery>) -> WebResult<Response> {
    if !automode::is_enabled(me.id) {
        return Ok(see_other("/views"));
    }
    let task = automode::next_task(&st.db, &st.presence, &me).await?;
    let polling = q.poll.is_some();
    match (task, polling) {
        (Some(id), true) => Ok(([("HX-Redirect", format!("/tickets/{id}"))], StatusCode::OK).into_response()),
        (Some(id), false) => Ok(match q.ok.as_deref().filter(|m| !m.is_empty()) {
            Some(msg) => see_other_ok(&format!("/tickets/{id}"), msg),
            None => see_other(&format!("/tickets/{id}")),
        }),
        (None, true) => Ok(StatusCode::NO_CONTENT.into_response()),
        (None, false) => {
            let all = automode::all_sources(&st.db).await?;
            let mine = automode::sources(&st.db, me.id).await?;
            let sources = mine.iter().filter_map(|k| all.iter().find(|(key, _)| key == k).map(|(_, l)| l.clone())).collect();
            Ok(page(WaitPage { nav: Nav::new(&me, "views"), sources })?.into_response())
        }
    }
}

/// Ticket überspringen: Hat der Auto-Modus es eben erst zugewiesen, wird die Zuweisung
/// gelöst; es kommt 30 Minuten lang nicht wieder.
async fn skip(State(st): State<AppState>, WebAgent(me): WebAgent, Path(id): Path<i64>) -> WebResult<Response> {
    if automode::take_claim(me.id, id) {
        crate::service::release_ticket(&st.db, &me, id).await?;
    }
    automode::skip(me.id, id);
    Ok(see_other("/auto/next"))
}
