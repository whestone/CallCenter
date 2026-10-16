use askama::Template;
use axum::{
    response::{Html, Response},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::extract::{Form, Path, State};
use super::*;
use crate::{
    auth::{WebAdmin, WebAgent},
    rules::{self, Action, Conditions},
    service::{self, UserInput},
    sla::{self as sla_mod, SlaMetric},
};

/// „Meine Makros“: persönliche Makros eines Agenten, gleicher Editor wie in der Verwaltung.
pub fn personal_router() -> Router<AppState> {
    Router::new()
        .route("/me/macros", get(my_macros).post(my_macro_create))
        .route("/me/macros/new", get(my_macro_new))
        .route("/me/macros/{id}", get(my_macro_edit).post(my_macro_update))
        .route("/me/macros/{id}/delete", post(my_macro_delete))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin", get(|| async { Redirect::to("/admin/settings") }))
        .route("/admin/settings", get(settings_index).post(save_settings))
        .route("/admin/statuses", get(statuses_index).post(save_statuses))
        .route("/admin/statuses/new", post(create_status))
        .route("/admin/statuses/{key}/delete", post(delete_status))
        .route("/admin/agents", get(agents_index).post(create_agent))
        .route("/admin/agents/{id}", get(agent_edit).post(update_agent))
        .route("/admin/agents/{id}/auto", post(save_auto_sources))
        .route("/admin/fields", get(fields_index).post(create_field))
        .route("/admin/fields/{id}", post(update_field))
        .route("/admin/fields/{id}/delete", post(delete_field))
        .route("/admin/hours", get(hours_index).post(save_hours))
        .route("/admin/hours/holidays", post(add_holiday))
        .route("/admin/hours/holidays/{day}/delete", post(delete_holiday))
        .route("/admin/skills", get(skills_index).post(create_skill))
        .route("/admin/skills/{id}", post(update_skill))
        .route("/admin/skills/{id}/delete", post(delete_skill))
        .route("/admin/groups", get(groups_index).post(create_group))
        .route("/admin/groups/{id}", post(update_group))
        .route("/admin/groups/{id}/delete", post(delete_group))
        .route("/admin/rules/{kind}", get(rules_index).post(create_rule))
        .route("/admin/rules/{kind}/new", get(rule_new))
        .route("/admin/rules/{kind}/{id}", get(rule_edit).post(update_rule))
        .route("/admin/rules/{kind}/{id}/delete", post(delete_rule))
}

// ---------------------------------------------------------------------------
// Status: Bezeichnung, Farbe, Reihenfolge, aktiv, erlaubte Übergänge
// ---------------------------------------------------------------------------

struct StatusRow {
    key: &'static str,
    label: &'static str,
    color: String,
    active: bool,
    tickets: i64,
    custom: bool,
    /// Grundart zur Auswahl (bei festen Status nur Anzeige).
    category_opts: Vec<Opt>,
    category_label: &'static str,
}

struct TransitionRow {
    from: &'static str,
    label: &'static str,
    /// (Zielschlüssel, erlaubt, gleich dem Ausgangsstatus)
    to: Vec<(&'static str, bool, bool)>,
}

#[derive(Template)]
#[template(path = "admin/statuses.html")]
struct StatusesPage {
    nav: Nav,
    tab: &'static str,
    error: String,
    rows: Vec<StatusRow>,
    heads: Vec<(&'static str, &'static str)>,
    matrix: Vec<TransitionRow>,
    /// Grundarten für „Status hinzufügen“.
    base_opts: Vec<Opt>,
}

#[derive(Deserialize)]
struct ErrorQuery {
    error: Option<String>,
}

async fn statuses_index(
    State(st): State<AppState>,
    WebAdmin(me): WebAdmin,
    super::extract::Query(q): super::extract::Query<ErrorQuery>,
) -> WebResult<Html<String>> {
    // Tickets je angezeigtem Status (eigener Status, sonst Grundart).
    let counts: std::collections::HashMap<String, i64> = sqlx::query_as::<_, (String, i64)>(
        "SELECT coalesce(substatus, status), count(*) FROM tickets GROUP BY 1",
    )
    .fetch_all(&st.db)
    .await?
    .into_iter()
    .collect();
    let bases: Vec<&str> = STATUSES.to_vec();
    let all = crate::statuses::all();
    page(StatusesPage {
        nav: Nav::new(&me, "admin"),
        tab: "statuses",
        error: q.error.unwrap_or_default(),
        rows: all
            .iter()
            .map(|s| StatusRow {
                key: s.key,
                label: s.label,
                color: s.color.clone(),
                active: s.active,
                tickets: counts.get(s.key).copied().unwrap_or(0),
                custom: s.custom,
                category_opts: bases
                    .iter()
                    .map(|b| Opt { value: b.to_string(), label: base_label(b).to_string(), selected: *b == s.category })
                    .collect(),
                category_label: base_label(s.category),
            })
            .collect(),
        base_opts: bases
            .iter()
            .filter(|b| **b != "new" && **b != "closed")
            .map(|b| Opt { value: b.to_string(), label: base_label(b).to_string(), selected: *b == "hold" })
            .collect(),
        heads: all.iter().map(|s| (s.key, s.label)).collect(),
        matrix: all
            .iter()
            .map(|f| TransitionRow {
                from: f.key,
                label: f.label,
                to: all.iter().map(|t| (t.key, f.key != t.key && crate::statuses::allowed(f.key, t.key), f.key == t.key)).collect(),
            })
            .collect(),
    })
}

async fn save_statuses(
    State(st): State<AppState>,
    WebAdmin(_): WebAdmin,
    Form(pairs): Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    let value = |name: &str| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default();
    let active: std::collections::HashSet<&str> = pairs.iter().filter(|(k, _)| k == "active").map(|(_, v)| v.as_str()).collect();
    let rows: Vec<(String, String, String, bool, String)> = pairs
        .iter()
        .filter(|(k, _)| k == "key")
        .map(|(_, key)| {
            let category = Some(value(&format!("category_{key}"))).filter(|c| !c.is_empty()).unwrap_or_else(|| key.clone());
            (key.clone(), value(&format!("label_{key}")), value(&format!("color_{key}")).to_lowercase(), active.contains(key.as_str()), category)
        })
        .collect();
    let transitions: Vec<(String, String)> = pairs
        .iter()
        .filter(|(k, _)| k == "t")
        .filter_map(|(_, v)| v.split_once('>').map(|(a, b)| (a.to_string(), b.to_string())))
        .collect();
    match crate::statuses::save(&st.db, &rows, &transitions).await {
        Ok(()) => Ok(see_other_ok("/admin/statuses", "Status gespeichert")),
        Err(AppError::Unprocessable(msg)) => Ok(see_other(&with_query("/admin/statuses", &[("error", &msg)]))),
        Err(e) => Err(e.into()),
    }
}

/// Bezeichnung der Grundart (feste Standardnamen, unabhängig von Umbenennungen).
fn base_label(key: &str) -> &'static str {
    match key {
        "new" => "Neu",
        "open" => "Offen",
        "pending" => "Wartend",
        "hold" => "Angehalten",
        "solved" => "Gelöst",
        "closed" => "Geschlossen",
        _ => "?",
    }
}

#[derive(Deserialize)]
struct NewStatusForm {
    label: String,
    category: String,
    color: String,
}

async fn create_status(State(st): State<AppState>, WebAdmin(_): WebAdmin, Form(f): Form<NewStatusForm>) -> WebResult<Response> {
    match crate::statuses::create(&st.db, &f.label, &f.category, &f.color.to_lowercase()).await {
        Ok(_) => Ok(see_other_ok("/admin/statuses", &format!("Status „{}“ angelegt", f.label.trim()))),
        Err(AppError::Unprocessable(msg)) => Ok(see_other(&with_query("/admin/statuses", &[("error", &msg)]))),
        Err(e) => Err(e.into()),
    }
}

async fn delete_status(State(st): State<AppState>, WebAdmin(_): WebAdmin, Path(key): Path<String>) -> WebResult<Response> {
    match crate::statuses::delete(&st.db, &key).await {
        Ok(()) => Ok(see_other_ok("/admin/statuses", "Status gelöscht")),
        Err(AppError::Unprocessable(msg)) => Ok(see_other(&with_query("/admin/statuses", &[("error", &msg)]))),
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------------------
// Allgemeine Einstellungen
// ---------------------------------------------------------------------------

struct SettingsSection {
    name: &'static str,
    fields: Vec<crate::config::FieldView>,
}

#[derive(Template)]
#[template(path = "admin/settings.html")]
struct SettingsPage {
    nav: Nav,
    tab: &'static str,
    sections: Vec<SettingsSection>,
    notice: String,
}

#[derive(Deserialize)]
struct SettingsQuery {
    notice: Option<String>,
}

async fn settings_index(WebAdmin(me): WebAdmin, super::extract::Query(q): super::extract::Query<SettingsQuery>) -> WebResult<Html<String>> {
    page(SettingsPage {
        nav: Nav::new(&me, "admin"),
        tab: "settings",
        sections: crate::config::GENERAL_SECTIONS
            .iter()
            .map(|name| SettingsSection { name, fields: crate::config::fields(name) })
            .collect(),
        notice: q.notice.unwrap_or_default(),
    })
}

async fn save_settings(
    State(st): State<AppState>,
    WebAdmin(_): WebAdmin,
    Form(pairs): Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    let section = pairs.iter().find(|(k, _)| k == "section").map(|(_, v)| v.as_str()).unwrap_or_default();
    let Some(section) = crate::config::GENERAL_SECTIONS.iter().find(|s| **s == section) else {
        return Err(AppError::invalid("Unbekannter Abschnitt").into());
    };
    let notice = match crate::config::save_section(&st.db, &[section], &pairs).await {
        Ok(()) => format!("{section}: gespeichert"),
        Err(AppError::Unprocessable(msg)) => msg,
        Err(e) => return Err(e.into()),
    };
    Ok(see_other(&with_query("/admin/settings", &[("notice", &notice)])))
}

// ---------------------------------------------------------------------------
// Agenten
// ---------------------------------------------------------------------------

struct AgentRow {
    id: i64,
    name: String,
    initials: String,
    email: String,
    role_label: &'static str,
    groups: String,
    active: bool,
    availability: String,
    availability_label: &'static str,
    load: String,
    skills: String,
    updated: String,
}

struct GroupCheck {
    id: i64,
    name: String,
    checked: bool,
}

#[derive(Template)]
#[template(path = "admin/agents.html")]
struct AgentsPage {
    nav: Nav,
    tab: &'static str,
    agents: Vec<AgentRow>,
    groups: Vec<GroupCheck>,
    acols: super::list_prefs::ListPrefs,
    q: String,
    role_opts: Vec<Opt>,
    availability_opts: Vec<Opt>,
    return_to: String,
}

#[derive(Deserialize)]
struct AgentsQuery {
    q: Option<String>,
    role: Option<String>,
    availability: Option<String>,
}

fn choice_opts(items: &[(&str, &str)], selected: &str) -> Vec<Opt> {
    items.iter().map(|(v, l)| Opt { value: v.to_string(), label: l.to_string(), selected: selected == *v }).collect()
}

async fn all_groups(db: &PgPool) -> AppResult<Vec<Group>> {
    Ok(sqlx::query_as("SELECT * FROM agent_groups ORDER BY name").fetch_all(db).await?)
}

async fn agents_index(
    State(st): State<AppState>,
    WebAdmin(me): WebAdmin,
    super::extract::Query(aq): super::extract::Query<AgentsQuery>,
) -> WebResult<Html<String>> {
    let acols = super::list_prefs::load_for(&st.db, me.id, &super::list_prefs::AGENTS).await?;
    let pick = |key: &str, v: Option<String>| if acols.filter(key) { v.unwrap_or_default().trim().to_string() } else { String::new() };
    let q = pick("q", aq.q);
    let role = pick("role", aq.role);
    let availability = pick("availability", aq.availability);
    #[allow(clippy::type_complexity)]
    let rows: Vec<(i64, String, Option<String>, String, bool, Option<String>, String, i32, i64, Option<String>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT u.id, u.name, u.email, u.role, u.active,
                (SELECT string_agg(g.name, ', ' ORDER BY g.name) FROM group_memberships m
                 JOIN agent_groups g ON g.id = m.group_id WHERE m.user_id = u.id),
                u.availability, u.capacity,
                (SELECT count(*) FROM tickets t WHERE t.assignee_id = u.id AND t.status IN ('new', 'open')),
                (SELECT string_agg(s.name, ', ' ORDER BY s.name) FROM user_skills us
                 JOIN skills s ON s.id = us.skill_id WHERE us.user_id = u.id),
                u.updated_at
         FROM users u WHERE u.role IN ('agent', 'admin') AND u.email <> $1
           AND ($2 = '' OR u.name ILIKE '%' || $2 || '%' OR u.email ILIKE '%' || $2 || '%')
           AND ($3 = '' OR u.role = $3)
           AND ($4 = '' OR u.availability = $4)
         ORDER BY u.active DESC, u.name",
    )
    .bind(crate::automations::SYSTEM_EMAIL)
    .bind(&q)
    .bind(&role)
    .bind(&availability)
    .fetch_all(&st.db)
    .await?;
    let now = chrono::Utc::now();
    page(AgentsPage {
        nav: Nav::new(&me, "admin"),
        tab: "agents",
        agents: rows
            .into_iter()
            .map(|(id, name, email, role, active, groups, availability, capacity, load, skills, updated)| AgentRow {
                id,
                updated: fmt_rel(updated, now),
                skills: skills.unwrap_or_default(),
                initials: initials(&name),
                name,
                email: email.unwrap_or_default(),
                role_label: role_label(&role),
                groups: groups.unwrap_or_default(),
                active,
                availability_label: availability_label(&availability),
                availability,
                load: format!("{load} / {capacity}"),
            })
            .collect(),
        groups: all_groups(&st.db)
            .await?
            .into_iter()
            .map(|g| GroupCheck { id: g.id, name: g.name, checked: false })
            .collect(),
        return_to: with_query("/admin/agents", &[("q", &q), ("role", &role), ("availability", &availability)]),
        role_opts: choice_opts(&[("", "Alle Rollen"), ("agent", "Agent"), ("admin", "Administrator")], &role),
        availability_opts: choice_opts(
            &[("", "Jede Erreichbarkeit"), ("online", "Online"), ("away", "Abwesend"), ("offline", "Offline")],
            &availability,
        ),
        q,
        acols,
    })
}

/// Formulare mit Mehrfachauswahl (group_id=1&group_id=2) als Schlüssel/Wert-Liste.
fn field<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn group_ids(pairs: &[(String, String)]) -> Vec<i64> {
    pairs.iter().filter(|(k, _)| k == "group_id").filter_map(|(_, v)| v.parse().ok()).collect()
}

async fn create_agent(
    State(st): State<AppState>,
    WebAdmin(me): WebAdmin,
    Form(pairs): Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    let user = service::create_user(
        &st.db,
        &me,
        UserInput {
            name: field(&pairs, "name").map(Into::into),
            email: field(&pairs, "email").map(Into::into),
            role: Some(field(&pairs, "role").unwrap_or("agent").into()),
            password: field(&pairs, "password").map(Into::into),
            ..Default::default()
        },
    )
    .await?;
    service::set_group_memberships(&st.db, user.id, &group_ids(&pairs)).await?;
    Ok(see_other_ok("/admin/agents", "Angelegt"))
}

#[derive(Template)]
#[template(path = "admin/agent_edit.html")]
struct AgentEditPage {
    nav: Nav,
    tab: &'static str,
    id: i64,
    name: String,
    email: String,
    phone: String,
    active: bool,
    capacity: i32,
    extension: String,
    twofa: bool,
    role_opts: Vec<Opt>,
    groups: Vec<GroupCheck>,
    skills: Vec<GroupCheck>,
    /// Auto-Modus: alle Quellen, eingestellte zuerst in ihrer Reihenfolge.
    auto_sources: Vec<(String, String, bool)>,
    auto_on: bool,
    /// Weiter nach diesen Status: (Schlüssel, Bezeichnung, an)
    auto_advance: Vec<(&'static str, &'static str, bool)>,
}

async fn agent_edit(State(st): State<AppState>, WebAdmin(me): WebAdmin, Path(id): Path<i64>) -> WebResult<Html<String>> {
    let u: User = sqlx::query_as("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let member_of: Vec<i64> = sqlx::query_scalar("SELECT group_id FROM group_memberships WHERE user_id = $1")
        .bind(id)
        .fetch_all(&st.db)
        .await?;
    page(AgentEditPage {
        nav: Nav::new(&me, "admin"),
        tab: "agents",
        id: u.id,
        name: u.name.clone(),
        email: u.email.clone().unwrap_or_default(),
        phone: u.phone.clone().unwrap_or_default(),
        active: u.active,
        capacity: u.capacity,
        extension: u.extension.clone().unwrap_or_default(),
        twofa: u.totp_secret.is_some(),
        role_opts: enum_opts(&ROLES, role_label, Some(&u.role), None),
        groups: all_groups(&st.db)
            .await?
            .into_iter()
            .map(|g| GroupCheck { checked: member_of.contains(&g.id), id: g.id, name: g.name })
            .collect(),
        skills: sqlx::query_as::<_, (i64, String, bool)>(
            "SELECT s.id, s.name, EXISTS(SELECT 1 FROM user_skills us WHERE us.skill_id = s.id AND us.user_id = $1)
             FROM skills s ORDER BY s.name",
        )
        .bind(id)
        .fetch_all(&st.db)
        .await?
        .into_iter()
        .map(|(id, name, checked)| GroupCheck { id, name, checked })
        .collect(),
        auto_sources: {
            let all = crate::automode::all_sources(&st.db).await?;
            let mine = crate::automode::sources(&st.db, id).await?;
            let mut list: Vec<(String, String, bool)> =
                mine.iter().filter_map(|k| all.iter().find(|(key, _)| key == k).map(|(k, l)| (k.clone(), l.clone(), true))).collect();
            list.extend(all.into_iter().filter(|(k, _)| !mine.contains(k)).map(|(k, l)| (k, l, false)));
            list
        },
        auto_on: crate::automode::is_enabled(id),
        auto_advance: {
            let on = crate::automode::advance_on(&st.db, id).await?;
            crate::automode::ADVANCE_CHOICES.iter().map(|(k, l)| (*k, *l, on.iter().any(|s| s == k))).collect()
        },
    })
}

/// Quellen des Auto-Modus speichern: Reihenfolge der Liste, nur eingeschaltete.
async fn save_auto_sources(
    State(st): State<AppState>,
    WebAdmin(_): WebAdmin,
    Path(id): Path<i64>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    let on: std::collections::HashSet<&str> = pairs.iter().filter(|(k, _)| k == "source").map(|(_, v)| v.as_str()).collect();
    let order: Vec<String> = pairs.iter().filter(|(k, v)| k == "order" && on.contains(v.as_str())).map(|(_, v)| v.clone()).collect();
    crate::automode::save_sources(&st.db, id, &order).await?;
    let advance: Vec<String> = pairs.iter().filter(|(k, _)| k == "advance").map(|(_, v)| v.clone()).collect();
    crate::automode::save_advance(&st.db, id, &advance).await?;
    Ok(see_other_ok(&format!("/admin/agents/{id}"), "Auto-Modus gespeichert"))
}

async fn update_agent(
    State(st): State<AppState>,
    WebAdmin(me): WebAdmin,
    Path(id): Path<i64>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    service::update_user(
        &st.db,
        &me,
        id,
        UserInput {
            name: field(&pairs, "name").map(Into::into),
            email: field(&pairs, "email").map(Into::into),
            phone: field(&pairs, "phone").map(Into::into),
            role: field(&pairs, "role").map(Into::into),
            active: Some(field(&pairs, "active").is_some()),
            password: field(&pairs, "password").map(Into::into),
            organization_id: None,
            availability: None,
            capacity: match field(&pairs, "capacity").map(str::trim).filter(|s| !s.is_empty()) {
                Some(c) => Some(c.parse().map_err(|_| AppError::invalid("Kapazität muss eine Zahl sein"))?),
                None => None,
            },
        },
    )
    .await?;
    service::set_group_memberships(&st.db, id, &group_ids(&pairs)).await?;
    let skill_ids: Vec<i64> = pairs.iter().filter(|(k, _)| k == "skill_id").filter_map(|(_, v)| v.parse().ok()).collect();
    sqlx::query("DELETE FROM user_skills WHERE user_id = $1").bind(id).execute(&st.db).await?;
    sqlx::query("INSERT INTO user_skills (user_id, skill_id) SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING")
        .bind(id)
        .bind(&skill_ids)
        .execute(&st.db)
        .await?;
    if let Some((_, ext)) = pairs.iter().find(|(k, _)| k == "extension") {
        sqlx::query("UPDATE users SET extension = NULLIF($2, '') WHERE id = $1")
            .bind(id)
            .bind(ext.trim())
            .execute(&st.db)
            .await?;
    }
    // Z. B. bei verlorenem Smartphone: 2FA zurücksetzen, der Agent richtet sie neu ein.
    if field(&pairs, "reset_2fa").is_some() {
        sqlx::query("UPDATE users SET totp_secret = NULL, totp_pending_secret = NULL WHERE id = $1")
            .bind(id)
            .execute(&st.db)
            .await?;
    }
    Ok(see_other_ok("/admin/agents", "Gespeichert"))
}

// ---------------------------------------------------------------------------
// Fähigkeiten (Routing nach Skills)
// ---------------------------------------------------------------------------

struct SkillRow {
    id: i64,
    name: String,
    tags: String,
    agents: String,
    open_tickets: i64,
}

#[derive(Template)]
#[template(path = "admin/skills.html")]
struct SkillsPage {
    nav: Nav,
    tab: &'static str,
    skills: Vec<SkillRow>,
    error: String,
}

async fn skills_page(db: &PgPool, me: &User, error: String) -> AppResult<SkillsPage> {
    #[allow(clippy::type_complexity)]
    let rows: Vec<(i64, String, Vec<String>, Option<String>, i64)> = sqlx::query_as(
        "SELECT s.id, s.name, s.tags,
                (SELECT string_agg(u.name, ', ' ORDER BY u.name) FROM user_skills us JOIN users u ON u.id = us.user_id
                 WHERE us.skill_id = s.id AND u.active),
                (SELECT count(*) FROM tickets t WHERE t.tags && s.tags AND t.status IN ('new', 'open', 'pending', 'hold'))
         FROM skills s ORDER BY s.name",
    )
    .fetch_all(db)
    .await?;
    Ok(SkillsPage {
        nav: Nav::new(me, "admin"),
        tab: "skills",
        skills: rows
            .into_iter()
            .map(|(id, name, tags, agents, open_tickets)| SkillRow {
                id,
                name,
                tags: tags.join(" "),
                agents: agents.unwrap_or_default(),
                open_tickets,
            })
            .collect(),
        error,
    })
}

async fn skills_index(State(st): State<AppState>, WebAdmin(me): WebAdmin) -> WebResult<Html<String>> {
    page(skills_page(&st.db, &me, String::new()).await?)
}

#[derive(Deserialize)]
struct SkillForm {
    name: String,
    #[serde(default)]
    tags: String,
}

fn skill_input(f: &SkillForm) -> AppResult<(String, Vec<String>)> {
    let name = f.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::invalid("Bitte einen Namen angeben"));
    }
    let tags = rules::normalize_tags(f.tags.split([' ', ',']));
    if tags.is_empty() {
        return Err(AppError::invalid("Bitte mindestens einen Tag angeben, an dem Tickets diese Fähigkeit erkennen"));
    }
    Ok((name, tags))
}

async fn save_skill(db: &PgPool, id: Option<i64>, f: &SkillForm) -> AppResult<()> {
    let (name, tags) = skill_input(f)?;
    let result = match id {
        Some(id) => sqlx::query("UPDATE skills SET name = $2, tags = $3 WHERE id = $1").bind(id).bind(&name).bind(&tags).execute(db).await,
        None => sqlx::query("INSERT INTO skills (name, tags) VALUES ($1, $2)").bind(&name).bind(&tags).execute(db).await,
    };
    match result {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err(AppError::invalid(format!("Es gibt bereits eine Fähigkeit „{name}“")))
        }
        other => {
            other?;
            Ok(())
        }
    }
}

async fn skill_result(st: &AppState, me: &User, r: AppResult<()>) -> WebResult<Response> {
    match r {
        Ok(()) => Ok(see_other("/admin/skills")),
        Err(AppError::Unprocessable(msg)) => {
            let html = page(skills_page(&st.db, me, msg).await?)?;
            Ok((axum::http::StatusCode::UNPROCESSABLE_ENTITY, html).into_response())
        }
        Err(e) => Err(e.into()),
    }
}

async fn create_skill(State(st): State<AppState>, WebAdmin(me): WebAdmin, Form(f): Form<SkillForm>) -> WebResult<Response> {
    let r = save_skill(&st.db, None, &f).await;
    skill_result(&st, &me, r).await
}

async fn update_skill(
    State(st): State<AppState>,
    WebAdmin(me): WebAdmin,
    Path(id): Path<i64>,
    Form(f): Form<SkillForm>,
) -> WebResult<Response> {
    let r = save_skill(&st.db, Some(id), &f).await;
    skill_result(&st, &me, r).await
}

async fn delete_skill(State(st): State<AppState>, WebAdmin(_): WebAdmin, Path(id): Path<i64>) -> WebResult<Response> {
    sqlx::query("DELETE FROM skills WHERE id = $1").bind(id).execute(&st.db).await?;
    Ok(see_other("/admin/skills"))
}

// ---------------------------------------------------------------------------
// Eigene Ticketfelder
// ---------------------------------------------------------------------------

struct FieldRow {
    id: i64,
    key: String,
    label: String,
    type_label: &'static str,
    is_dropdown: bool,
    options: String,
    required: bool,
    active: bool,
    position: i32,
}

#[derive(Template)]
#[template(path = "admin/fields.html")]
struct FieldsPage {
    nav: Nav,
    tab: &'static str,
    fields: Vec<FieldRow>,
    type_opts: Vec<Opt>,
}

async fn fields_index(State(st): State<AppState>, WebAdmin(me): WebAdmin) -> WebResult<Html<String>> {
    let fields = service::ticket_fields(&st.db, false).await?;
    page(FieldsPage {
        nav: Nav::new(&me, "admin"),
        tab: "fields",
        fields: fields
            .into_iter()
            .map(|f| FieldRow {
                id: f.id,
                type_label: field_type_label(&f.field_type),
                is_dropdown: f.field_type == "dropdown",
                options: f.options.join(", "),
                key: f.key,
                label: f.label,
                required: f.required_on_solve,
                active: f.active,
                position: f.position,
            })
            .collect(),
        type_opts: enum_opts(&FIELD_TYPES, field_type_label, Some("text"), None),
    })
}

fn field_input(pairs: &[(String, String)]) -> service::FieldInput {
    service::FieldInput {
        key: field(pairs, "key").unwrap_or_default().into(),
        label: field(pairs, "label").unwrap_or_default().into(),
        field_type: field(pairs, "field_type").unwrap_or("text").into(),
        options: field(pairs, "options").unwrap_or_default().split([',', '\n']).map(str::to_string).collect(),
        required_on_solve: field(pairs, "required_on_solve").is_some(),
        position: field(pairs, "position").and_then(|p| p.trim().parse().ok()).unwrap_or(0),
        active: field(pairs, "active").is_some(),
    }
}

async fn create_field(State(st): State<AppState>, WebAdmin(_): WebAdmin, Form(pairs): Form<Vec<(String, String)>>) -> WebResult<Response> {
    let mut input = field_input(&pairs);
    input.active = true;
    service::create_field(&st.db, input).await?;
    Ok(see_other("/admin/fields"))
}

async fn update_field(
    State(st): State<AppState>,
    WebAdmin(_): WebAdmin,
    Path(id): Path<i64>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    service::update_field(&st.db, id, field_input(&pairs)).await?;
    Ok(see_other("/admin/fields"))
}

async fn delete_field(State(st): State<AppState>, WebAdmin(_): WebAdmin, Path(id): Path<i64>) -> WebResult<Response> {
    sqlx::query("DELETE FROM ticket_fields WHERE id = $1").bind(id).execute(&st.db).await?;
    Ok(see_other("/admin/fields"))
}

// ---------------------------------------------------------------------------
// Geschäftszeiten & Feiertage
// ---------------------------------------------------------------------------

use chrono::Datelike;

struct DayRow {
    weekday: i32,
    name: &'static str,
    enabled: bool,
    opens: String,
    closes: String,
}

struct HolidayRow {
    day: String,
    label: String,
    name: String,
}

#[derive(Template)]
#[template(path = "admin/hours.html")]
struct HoursPage {
    nav: Nav,
    tab: &'static str,
    days: Vec<DayRow>,
    holidays: Vec<HolidayRow>,
    notice: String,
}

const WEEKDAYS: [&str; 7] = ["Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag", "Sonntag"];

#[derive(Deserialize)]
struct NoticeQuery {
    notice: Option<String>,
}

async fn hours_index(
    State(st): State<AppState>,
    WebAdmin(me): WebAdmin,
    super::extract::Query(q): super::extract::Query<NoticeQuery>,
) -> WebResult<Html<String>> {
    let days: Vec<(i32, bool, chrono::NaiveTime, chrono::NaiveTime)> =
        sqlx::query_as("SELECT weekday, enabled, opens_at, closes_at FROM business_hours ORDER BY weekday")
            .fetch_all(&st.db)
            .await?;
    let holidays: Vec<(chrono::NaiveDate, String)> =
        sqlx::query_as("SELECT day, name FROM holidays WHERE day >= current_date - 30 ORDER BY day")
            .fetch_all(&st.db)
            .await?;
    page(HoursPage {
        nav: Nav::new(&me, "admin"),
        tab: "hours",
        days: days
            .into_iter()
            .map(|(weekday, enabled, opens, closes)| DayRow {
                name: WEEKDAYS[(weekday - 1).clamp(0, 6) as usize],
                weekday,
                enabled,
                opens: opens.format("%H:%M").to_string(),
                closes: closes.format("%H:%M").to_string(),
            })
            .collect(),
        holidays: holidays
            .into_iter()
            .map(|(day, name)| HolidayRow {
                day: day.format("%Y-%m-%d").to_string(),
                label: format!(
                    "{} {}",
                    ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"][day.weekday().num_days_from_monday() as usize],
                    day.format("%d.%m.%Y")
                ),
                name,
            })
            .collect(),
        notice: q.notice.unwrap_or_default(),
    })
}

async fn save_hours(State(st): State<AppState>, WebAdmin(_): WebAdmin, Form(pairs): Form<Vec<(String, String)>>) -> WebResult<Response> {
    let mut tx = st.db.begin().await?;
    for weekday in 1..=7 {
        let enabled = field(&pairs, &format!("enabled_{weekday}")).is_some();
        let time = |key: &str| -> AppResult<chrono::NaiveTime> {
            let raw = field(&pairs, &format!("{key}_{weekday}")).unwrap_or_default();
            chrono::NaiveTime::parse_from_str(raw.trim(), "%H:%M")
                .map_err(|_| AppError::invalid(format!("Ungültige Uhrzeit bei {}", WEEKDAYS[weekday as usize - 1])))
        };
        let (opens, closes) = (time("opens")?, time("closes")?);
        if closes <= opens {
            return Err(AppError::invalid(format!("{}: Ende muss nach Beginn liegen", WEEKDAYS[weekday as usize - 1])).into());
        }
        sqlx::query("UPDATE business_hours SET enabled = $2, opens_at = $3, closes_at = $4 WHERE weekday = $1")
            .bind(weekday)
            .bind(enabled)
            .bind(opens)
            .bind(closes)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(see_other("/admin/hours?notice=Gesch%C3%A4ftszeiten+gespeichert"))
}

#[derive(Deserialize)]
struct HolidayForm {
    day: String,
    name: String,
}

async fn add_holiday(State(st): State<AppState>, WebAdmin(_): WebAdmin, Form(f): Form<HolidayForm>) -> WebResult<Response> {
    let day = chrono::NaiveDate::parse_from_str(f.day.trim(), "%Y-%m-%d").map_err(|_| AppError::invalid("Ungültiges Datum"))?;
    let name = f.name.trim();
    if name.is_empty() {
        return Err(AppError::invalid("Bitte einen Namen angeben").into());
    }
    sqlx::query("INSERT INTO holidays (day, name) VALUES ($1, $2) ON CONFLICT (day) DO UPDATE SET name = $2")
        .bind(day)
        .bind(name)
        .execute(&st.db)
        .await?;
    Ok(see_other("/admin/hours"))
}

async fn delete_holiday(State(st): State<AppState>, WebAdmin(_): WebAdmin, Path(day): Path<String>) -> WebResult<Response> {
    let day = chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d").map_err(|_| AppError::NotFound)?;
    sqlx::query("DELETE FROM holidays WHERE day = $1").bind(day).execute(&st.db).await?;
    Ok(see_other("/admin/hours"))
}

// ---------------------------------------------------------------------------
// Gruppen
// ---------------------------------------------------------------------------

struct GroupRow {
    id: i64,
    name: String,
    description: String,
    members: i64,
    online: i64,
    queued: i64,
    assignment_opts: Vec<Opt>,
}

#[derive(Template)]
#[template(path = "admin/groups.html")]
struct GroupsPage {
    nav: Nav,
    tab: &'static str,
    groups: Vec<GroupRow>,
}

async fn groups_index(State(st): State<AppState>, WebAdmin(me): WebAdmin) -> WebResult<Html<String>> {
    let rows: Vec<(i64, String, String, String, i64, i64, i64)> = sqlx::query_as(
        "SELECT g.id, g.name, g.description, g.assignment,
                (SELECT count(*) FROM group_memberships m WHERE m.group_id = g.id),
                (SELECT count(*) FROM group_memberships m JOIN users u ON u.id = m.user_id
                 WHERE m.group_id = g.id AND u.active AND u.availability = 'online'),
                (SELECT count(*) FROM tickets t WHERE t.group_id = g.id AND t.assignee_id IS NULL
                 AND t.status IN ('new', 'open'))
         FROM agent_groups g ORDER BY g.name",
    )
    .fetch_all(&st.db)
    .await?;
    page(GroupsPage {
        nav: Nav::new(&me, "admin"),
        tab: "groups",
        groups: rows
            .into_iter()
            .map(|(id, name, description, assignment, members, online, queued)| GroupRow {
                id,
                name,
                description,
                members,
                online,
                queued,
                assignment_opts: enum_opts(&ASSIGNMENTS, assignment_label, Some(&assignment), None),
            })
            .collect(),
    })
}

#[derive(Deserialize)]
struct GroupForm {
    name: String,
    #[serde(default)]
    description: String,
}

async fn create_group(State(st): State<AppState>, WebAdmin(_): WebAdmin, Form(f): Form<GroupForm>) -> WebResult<Response> {
    service::create_group(&st.db, &f.name, &f.description).await?;
    Ok(see_other("/admin/groups"))
}

#[derive(Deserialize)]
struct AssignmentForm {
    assignment: String,
}

async fn update_group(
    State(st): State<AppState>,
    WebAdmin(_): WebAdmin,
    Path(id): Path<i64>,
    Form(f): Form<AssignmentForm>,
) -> WebResult<Response> {
    service::set_group_assignment(&st.db, id, &f.assignment).await?;
    Ok(see_other("/admin/groups"))
}

async fn delete_group(State(st): State<AppState>, WebAdmin(_): WebAdmin, Path(id): Path<i64>) -> WebResult<Response> {
    sqlx::query("DELETE FROM agent_groups WHERE id = $1").bind(id).execute(&st.db).await?;
    Ok(see_other("/admin/groups"))
}

// ---------------------------------------------------------------------------
// Business-Regeln: Views, Makros, Triggers, SLAs (gemeinsamer JSON-Editor)
// ---------------------------------------------------------------------------

struct RuleKind {
    key: &'static str,
    table: &'static str,
    title: &'static str,
    singular: &'static str,
    a_col: &'static str,
    a_label: &'static str,
    a_default: &'static str,
    b_col: Option<&'static str>,
    b_label: &'static str,
    b_default: &'static str,
}

const KINDS: [RuleKind; 5] = [
    RuleKind {
        key: "views",
        table: "views",
        title: "Ansichten",
        singular: "Ansicht",
        a_col: "conditions",
        a_label: "Bedingungen",
        a_default: "{\n  \"all\": [\n    { \"field\": \"status\", \"operator\": \"less_than\", \"value\": \"solved\" }\n  ],\n  \"any\": []\n}",
        b_col: Some("execution"),
        b_label: "Sortierung",
        b_default: "{ \"sort_by\": \"updated_at\", \"sort_order\": \"desc\" }",
    },
    RuleKind {
        key: "macros",
        table: "macros",
        title: "Makros",
        singular: "Makro",
        a_col: "actions",
        a_label: "Aktionen",
        a_default: "[\n  { \"field\": \"status\", \"value\": \"pending\" },\n  { \"field\": \"comment_value\", \"value\": \"Hallo, …\" }\n]",
        b_col: None,
        b_label: "",
        b_default: "",
    },
    RuleKind {
        key: "triggers",
        table: "triggers",
        title: "Trigger",
        singular: "Trigger",
        a_col: "conditions",
        a_label: "Bedingungen",
        a_default: "{\n  \"all\": [\n    { \"field\": \"update_type\", \"operator\": \"is\", \"value\": \"Create\" }\n  ],\n  \"any\": []\n}",
        b_col: Some("actions"),
        b_label: "Aktionen",
        b_default: "[\n  { \"field\": \"add_tags\", \"value\": \"neu\" }\n]",
    },
    RuleKind {
        key: "automations",
        table: "automations",
        title: "Automationen",
        singular: "Automation",
        a_col: "conditions",
        a_label: "Bedingungen",
        a_default: "{\n  \"all\": [\n    { \"field\": \"status\", \"operator\": \"is\", \"value\": \"pending\" },\n    { \"field\": \"hours_since_updated\", \"operator\": \"greater_than\", \"value\": 48 }\n  ],\n  \"any\": []\n}",
        b_col: Some("actions"),
        b_label: "Aktionen",
        b_default: "[\n  { \"field\": \"add_tags\", \"value\": \"nachfassen\" }\n]",
    },    RuleKind {
        key: "sla",
        table: "sla_policies",
        title: "SLA-Richtlinien",
        singular: "SLA-Richtlinie",
        a_col: "filter",
        a_label: "Filter (welche Tickets?)",
        a_default: "{ \"all\": [], \"any\": [] }",
        b_col: Some("metrics"),
        b_label: "Zielzeiten",
        b_default: "[\n  { \"priority\": \"urgent\", \"metric\": \"first_reply_time\", \"target\": 30 },\n  { \"priority\": \"urgent\", \"metric\": \"full_resolution_time\", \"target\": 240 }\n]",
    },
];

fn kind(key: &str) -> AppResult<&'static RuleKind> {
    KINDS.iter().find(|k| k.key == key).ok_or(AppError::NotFound)
}

struct RuleRow {
    id: i64,
    title: String,
    description: String,
    position: i32,
    active: bool,
}

#[derive(Template)]
#[template(path = "admin/rules.html")]
struct RulesPage {
    nav: Nav,
    tab: &'static str,
    title: &'static str,
    singular: &'static str,
    rules: Vec<RuleRow>,
    personal: bool,
    base: String,
}

async fn rules_index(State(st): State<AppState>, WebAdmin(me): WebAdmin, Path(key): Path<String>) -> WebResult<Html<String>> {
    let k = kind(&key)?;
    // Persönliche Makros der Agenten erscheinen hier nicht.
    let only_shared = if k.key == "macros" { "WHERE owner_id IS NULL" } else { "" };
    let rows: Vec<(i64, String, String, i32, bool)> = sqlx::query_as(&format!(
        "SELECT id, title, description, position, active FROM {} {only_shared} ORDER BY position, id",
        k.table
    ))
    .fetch_all(&st.db)
    .await?;
    page(RulesPage {
        nav: Nav::new(&me, "admin"),
        tab: k.key,
        title: k.title,
        singular: k.singular,
        rules: rows
            .into_iter()
            .map(|(id, title, description, position, active)| RuleRow { id, title, description, position, active })
            .collect(),
        personal: false,
        base: format!("/admin/rules/{}", k.key),
    })
}

#[derive(Template)]
#[template(path = "admin/rule_edit.html")]
struct RuleEditPage {
    nav: Nav,
    tab: &'static str,
    singular: &'static str,
    action: String,
    is_new: bool,
    error: String,
    title: String,
    description: String,
    position: i32,
    active: bool,
    a_label: &'static str,
    a_json: String,
    has_b: bool,
    b_label: &'static str,
    b_json: String,
    fields_help: String,
    a_builder: &'static str,
    b_builder: &'static str,
    meta_json: String,
    personal: bool,
    /// SLA: (Status, Bezeichnung, pausiert)
    pause_opts: Vec<(&'static str, &'static str, bool)>,
}

/// Welcher grafische Editor für Feld A/B eines Regeltyps zuständig ist.
fn builders(k: &RuleKind) -> (&'static str, &'static str) {
    match k.key {
        "views" => ("conditions", "execution"),
        "macros" => ("actions", ""),
        "triggers" | "automations" => ("conditions", "actions"),
        "sla" => ("conditions", "metrics"),
        _ => ("", ""),
    }
}

/// Feld-/Operator-/Wertedefinitionen für den grafischen Regel-Editor (static/rules.js).
async fn rule_meta(db: &PgPool) -> AppResult<String> {
    let agents: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM users WHERE role IN ('agent', 'admin') AND active ORDER BY name")
            .fetch_all(db)
            .await?;
    let groups: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM agent_groups ORDER BY name").fetch_all(db).await?;
    let orgs: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM organizations ORDER BY name").fetch_all(db).await?;

    let enum_opts = |values: &[&str], label: fn(&str) -> &'static str| -> Vec<Value> {
        values.iter().map(|v| json!([v, label(v)])).collect()
    };
    let id_opts = |rows: &[(i64, String)]| -> Vec<Value> { rows.iter().map(|(id, n)| json!([id.to_string(), n])).collect() };
    let mut agent_opts = vec![json!(["current_user", "(aktueller Agent)"])];
    agent_opts.extend(id_opts(&agents));

    let changes = ["changed", "changed_to", "changed_from", "not_changed"];
    let ops = |base: &[&'static str], with_changes: bool| -> Vec<&'static str> {
        let mut v = base.to_vec();
        if with_changes {
            v.extend(changes);
        }
        v
    };
    let cond = |key: &str, label: &str, ty: &str, options: Vec<Value>, operators: Vec<&str>| {
        json!({ "key": key, "label": label, "type": ty, "options": options, "ops": operators })
    };
    let condition_fields = vec![
        cond("update_type", "Ereignis", "enum", vec![json!(["Create", "Ticket erstellt"]), json!(["Change", "Ticket geändert"])], vec!["is"]),
        cond(
            "status",
            "Status",
            "enum",
            enum_opts(&crate::statuses::all().iter().map(|s| s.key).collect::<Vec<_>>(), status_label),
            ops(&["is", "is_not", "less_than", "greater_than"], true),
        ),
        cond("priority", "Priorität", "enum", enum_opts(&PRIORITIES, priority_label), ops(&["is", "is_not", "less_than", "greater_than", "present", "not_present"], true)),
        cond("type", "Typ", "enum", enum_opts(&TYPES, type_label), ops(&["is", "is_not", "present", "not_present"], true)),
        cond("channel", "Kanal", "enum", enum_opts(&CHANNELS, channel_label), vec!["is", "is_not"]),
        cond("assignee_id", "Bearbeiter", "id", agent_opts.clone(), ops(&["is", "is_not", "present", "not_present"], true)),
        cond("group_id", "Gruppe", "id", id_opts(&groups), ops(&["is", "is_not", "present", "not_present"], true)),
        cond("organization_id", "Organisation", "id", id_opts(&orgs), vec!["is", "is_not", "present", "not_present"]),
        cond("tags", "Tags", "tags", vec![], vec!["includes", "not_includes"]),
        cond("subject", "Betreff", "text", vec![], vec!["contains", "not_contains"]),
        cond("description", "Beschreibung", "text", vec![], vec!["contains", "not_contains"]),
        cond("comment_text", "Kommentartext", "text", vec![], vec!["contains", "not_contains"]),
        cond("comment_is_public", "Kommentar ist öffentlich", "bool", vec![json!([true, "ja"]), json!([false, "nein"])], vec!["is"]),
        cond("current_user_role", "Rolle des Bearbeitenden", "enum", enum_opts(&ROLES, role_label), vec!["is", "is_not"]),
        cond("hours_since_created", "Stunden seit Erstellung", "number", vec![], vec!["greater_than", "less_than"]),
        cond("hours_since_updated", "Stunden seit Aktualisierung", "number", vec![], vec!["greater_than", "less_than"]),
        cond("hours_since_solved", "Stunden seit Lösung", "number", vec![], vec!["greater_than", "less_than"]),
    ];

    let mut condition_fields = condition_fields;
    let custom = crate::service::ticket_fields(db, true).await?;
    let custom_type = |f: &TicketField| -> (&'static str, Vec<Value>) {
        match f.field_type.as_str() {
            "dropdown" => ("enum", f.options.iter().map(|o| json!([o, o])).collect()),
            "checkbox" => ("bool", vec![json!([true, "ja"]), json!([false, "nein"])]),
            "number" => ("number", vec![]),
            _ => ("text", vec![]),
        }
    };
    for f in &custom {
        let (ty, options) = custom_type(f);
        let operators = match ty {
            "number" => vec!["is", "greater_than", "less_than", "present", "not_present"],
            "text" => vec!["is", "is_not", "contains", "not_contains", "present", "not_present"],
            _ => vec!["is", "is_not", "present", "not_present", "changed", "changed_to"],
        };
        condition_fields.push(cond(&format!("custom_fields.{}", f.key), &format!("Feld: {}", f.label), ty, options, operators));
    }

    let mut opt_agent = vec![json!(["", "(Zuweisung entfernen)"])];
    opt_agent.extend(agent_opts);
    let mut opt_group = vec![json!(["", "(keine Gruppe)"])];
    opt_group.extend(id_opts(&groups));
    let mut opt_prio = vec![json!(["", "(keine)"])];
    opt_prio.extend(enum_opts(&PRIORITIES, priority_label));
    let mut opt_type = vec![json!(["", "(kein)"])];
    opt_type.extend(enum_opts(&TYPES, type_label));
    let act = |key: &str, label: &str, ty: &str, options: Vec<Value>| json!({ "key": key, "label": label, "type": ty, "options": options });
    let action_fields = vec![
        act(
            "status",
            "Status setzen",
            "enum",
            enum_opts(&crate::statuses::all().iter().filter(|s| s.category != "new").map(|s| s.key).collect::<Vec<_>>(), status_label),
        ),
        act("priority", "Priorität setzen", "enum", opt_prio),
        act("type", "Typ setzen", "enum", opt_type),
        act("assignee_id", "Bearbeiter setzen", "id", opt_agent),
        act("group_id", "Gruppe setzen", "id", opt_group),
        act("add_tags", "Tags hinzufügen", "tags", vec![]),
        act("remove_tags", "Tags entfernen", "tags", vec![]),
        act("set_tags", "Tags ersetzen", "tags", vec![]),
        act("subject", "Betreff setzen", "text", vec![]),
        act("comment_value", "Öffentliche Antwort", "longtext", vec![]),
        act("comment_private", "Interne Notiz", "longtext", vec![]),
        act("notify_requester", "Anfragenden benachrichtigen", "longtext", vec![]),
    ];
    let mut action_fields = action_fields;
    for f in &custom {
        let (ty, mut options) = custom_type(f);
        if ty == "enum" {
            options.insert(0, json!(["", "(leeren)"]));
        }
        action_fields.push(act(&format!("custom_fields.{}", f.key), &format!("Feld setzen: {}", f.label), ty, options));
    }

    let operator_labels = json!({
        "is": "ist", "is_not": "ist nicht", "less_than": "kleiner als", "greater_than": "größer als",
        "includes": "enthält einen von", "not_includes": "enthält keinen von", "contains": "enthält",
        "not_contains": "enthält nicht", "present": "ist gesetzt", "not_present": "ist leer",
        "changed": "wurde geändert", "changed_to": "geändert zu", "changed_from": "geändert von", "not_changed": "unverändert"
    });
    let meta = json!({
        "conditionFields": condition_fields,
        "actionFields": action_fields,
        "operatorLabels": operator_labels,
        "noValueOps": ["present", "not_present", "changed", "not_changed"],
        "sortFields": [["updated_at", "Aktualisiert"], ["created_at", "Erstellt"], ["priority", "Priorität"], ["status", "Status"], ["id", "Ticket-Nr."], ["due_at", "Fälligkeit"]],
        "priorities": enum_opts(&PRIORITIES, priority_label),
        "metrics": [["first_reply_time", "Erste Antwort"], ["full_resolution_time", "Lösung"]],
    });
    // In <script type="application/json"> darf kein "</" vorkommen.
    Ok(meta.to_string().replace("</", "<\\/"))
}

fn fields_help(k: &RuleKind) -> String {
    match k.key {
        "macros" => format!("Aktionsfelder: {}", rules::ACTION_FIELDS.join(", ")),
        "triggers" | "automations" => format!(
            "Bedingungsfelder: {} · Operatoren: {} · Aktionsfelder: {}",
            rules::CONDITION_FIELDS.join(", "),
            rules::OPERATORS.join(", "),
            rules::ACTION_FIELDS.join(", ")
        ),
        _ => format!(
            "Bedingungsfelder: {} · Operatoren: {}",
            rules::CONDITION_FIELDS.join(", "),
            rules::OPERATORS.join(", ")
        ),
    }
}

fn edit_page(me: &User, k: &'static RuleKind, id: Option<i64>, form: RuleForm, error: String, meta_json: String) -> RuleEditPage {
    let (a_builder, b_builder) = builders(k);
    let paused = form.pause_statuses();
    let pause_opts = if k.key == "sla" {
        crate::statuses::all()
            .into_iter()
            .filter(|s| !matches!(s.category, "solved" | "closed"))
            .map(|s| (s.key, s.label, paused.iter().any(|p| p == s.key)))
            .collect()
    } else {
        Vec::new()
    };
    RuleEditPage {
        pause_opts,
        a_builder,
        b_builder,
        meta_json,
        nav: Nav::new(me, "admin"),
        tab: k.key,
        singular: k.singular,
        action: match id {
            Some(id) => format!("/admin/rules/{}/{id}", k.key),
            None => format!("/admin/rules/{}", k.key),
        },
        is_new: id.is_none(),
        error,
        title: form.title,
        description: form.description,
        position: form.position,
        active: form.active.is_some(),
        a_label: k.a_label,
        a_json: form.a,
        has_b: k.b_col.is_some(),
        b_label: k.b_label,
        b_json: form.b.unwrap_or_default(),
        fields_help: fields_help(k),
        personal: false,
    }
}

async fn rule_new(State(st): State<AppState>, WebAdmin(me): WebAdmin, Path(key): Path<String>) -> WebResult<Html<String>> {
    let k = kind(&key)?;
    let form = RuleForm {
        title: String::new(),
        description: String::new(),
        position: 0,
        active: Some("on".into()),
        a: k.a_default.into(),
        b: k.b_col.map(|_| k.b_default.into()),
        extra: [("pause_pending".to_string(), "on".to_string())].into_iter().collect(),
    };
    page(edit_page(&me, k, None, form, String::new(), rule_meta(&st.db).await?))
}

async fn rule_edit(
    State(st): State<AppState>,
    WebAdmin(me): WebAdmin,
    Path((key, id)): Path<(String, i64)>,
) -> WebResult<Html<String>> {
    let k = kind(&key)?;
    let b_col = k.b_col.unwrap_or("NULL::jsonb");
    let row: (String, String, i32, bool, Value, Option<Value>) = sqlx::query_as(&format!(
        "SELECT title, description, position, active, {}, {} FROM {} WHERE id = $1",
        k.a_col, b_col, k.table
    ))
    .bind(id)
    .fetch_optional(&st.db)
    .await?
    .ok_or(AppError::NotFound)?;
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default();
    let mut form = RuleForm {
        title: row.0,
        description: row.1,
        position: row.2,
        active: row.3.then(|| "on".into()),
        a: pretty(&row.4),
        b: row.5.as_ref().map(pretty),
        extra: Default::default(),
    };
    if k.key == "sla" {
        let pause: sqlx::types::Json<Vec<String>> =
            sqlx::query_scalar("SELECT pause_statuses FROM sla_policies WHERE id = $1").bind(id).fetch_one(&st.db).await?;
        form.set_pause(&pause.0);
    }
    page(edit_page(&me, k, Some(id), form, String::new(), rule_meta(&st.db).await?))
}

#[derive(Deserialize, Clone, Default)]
struct RuleForm {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default, deserialize_with = "empty_as_zero")]
    position: i32,
    active: Option<String>,
    a: String,
    b: Option<String>,
    /// Weitere Felder, u. a. SLA-Pause: `pause_<status>` = an (auch eigene Status).
    #[serde(flatten)]
    extra: std::collections::HashMap<String, String>,
}

impl RuleForm {
    fn pause_statuses(&self) -> Vec<String> {
        crate::statuses::all()
            .into_iter()
            .filter(|s| self.extra.contains_key(&format!("pause_{}", s.key)))
            .map(|s| s.key.to_string())
            .collect()
    }

    fn set_pause(&mut self, list: &[String]) {
        self.extra.retain(|k, _| !k.starts_with("pause_"));
        for key in list {
            self.extra.insert(format!("pause_{key}"), "on".into());
        }
    }
}

fn empty_as_zero<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i32, D::Error> {
    let s = String::deserialize(d)?;
    if s.trim().is_empty() {
        return Ok(0);
    }
    s.trim().parse().map_err(|_| serde::de::Error::custom("Position muss eine ganze Zahl sein"))
}

/// JSON parsen und je nach Regeltyp fachlich prüfen.
fn validate_rule(k: &RuleKind, form: &RuleForm) -> Result<(Value, Option<Value>), String> {
    if form.title.trim().is_empty() {
        return Err("Titel darf nicht leer sein".into());
    }
    let a: Value = serde_json::from_str(&form.a).map_err(|e| format!("{}: ungültiges JSON ({e})", k.a_label))?;
    let b: Option<Value> = match (k.b_col, &form.b) {
        (Some(_), Some(text)) => {
            Some(serde_json::from_str(text).map_err(|e| format!("{}: ungültiges JSON ({e})", k.b_label))?)
        }
        (Some(_), None) => return Err(format!("{} fehlt", k.b_label)),
        _ => None,
    };
    let conditions = |v: &Value| -> Result<(), String> {
        serde_json::from_value::<Conditions>(v.clone()).map_err(|e| e.to_string())?.validate()
    };
    let actions = |v: &Value| -> Result<(), String> {
        rules::validate_actions(&serde_json::from_value::<Vec<Action>>(v.clone()).map_err(|e| e.to_string())?)
    };
    match k.key {
        "views" => {
            conditions(&a)?;
            let ex: ViewExecution =
                serde_json::from_value(b.clone().unwrap_or_default()).map_err(|e| e.to_string())?;
            if !VIEW_SORT_FIELDS.contains(&ex.sort_by.as_str()) {
                return Err(format!("Unbekanntes Sortierfeld: {}", ex.sort_by));
            }
        }
        "macros" => actions(&a)?,
        "triggers" | "automations" => {
            conditions(&a)?;
            actions(b.as_ref().unwrap_or(&Value::Null))?;
        }
        "sla" => {
            conditions(&a)?;
            let metrics: Vec<SlaMetric> =
                serde_json::from_value(b.clone().unwrap_or_default()).map_err(|e| e.to_string())?;
            sla_mod::validate_metrics(&metrics)?;
        }
        _ => {}
    }
    Ok((a, b))
}

async fn save_rule(db: &PgPool, k: &RuleKind, id: Option<i64>, form: &RuleForm, a: Value, b: Option<Value>) -> AppResult<()> {
    let saved = save_rule_row(db, k, id, form, a, b).await?;
    if k.key == "sla" {
        sqlx::query("UPDATE sla_policies SET pause_statuses = $1 WHERE id = $2")
            .bind(sqlx::types::Json(form.pause_statuses()))
            .bind(saved)
            .execute(db)
            .await?;
    }
    Ok(())
}

async fn save_rule_row(db: &PgPool, k: &RuleKind, id: Option<i64>, form: &RuleForm, a: Value, b: Option<Value>) -> AppResult<i64> {
    let cols = match k.b_col {
        Some(b_col) => format!("{}, {}", k.a_col, b_col),
        None => k.a_col.to_string(),
    };
    let sql = match (id, k.b_col) {
        (None, Some(_)) => format!("INSERT INTO {} (title, description, position, active, {cols}) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id", k.table),
        (None, None) => format!("INSERT INTO {} (title, description, position, active, {cols}) VALUES ($1, $2, $3, $4, $5) RETURNING id", k.table),
        (Some(_), Some(b_col)) => format!(
            "UPDATE {} SET title = $1, description = $2, position = $3, active = $4, {} = $5, {b_col} = $6, updated_at = now() WHERE id = $7 RETURNING id",
            k.table, k.a_col
        ),
        (Some(_), None) => format!(
            "UPDATE {} SET title = $1, description = $2, position = $3, active = $4, {} = $5, updated_at = now() WHERE id = $6 RETURNING id",
            k.table, k.a_col
        ),
    };
    let mut q = sqlx::query_scalar::<_, i64>(&sql)
        .bind(form.title.trim())
        .bind(form.description.trim())
        .bind(form.position)
        .bind(form.active.is_some())
        .bind(a);
    if k.b_col.is_some() {
        q = q.bind(b);
    }
    if let Some(id) = id {
        q = q.bind(id);
    }
    Ok(q.fetch_one(db).await?)
}

async fn create_rule(
    State(st): State<AppState>,
    WebAdmin(me): WebAdmin,
    Path(key): Path<String>,
    Form(form): Form<RuleForm>,
) -> WebResult<Response> {
    let k = kind(&key)?;
    match validate_rule(k, &form) {
        Ok((a, b)) => {
            save_rule(&st.db, k, None, &form, a, b).await?;
            Ok(see_other(&format!("/admin/rules/{}", k.key)))
        }
        Err(e) => Ok(page(edit_page(&me, k, None, form, e, rule_meta(&st.db).await?))?.into_response()),
    }
}

async fn update_rule(
    State(st): State<AppState>,
    WebAdmin(me): WebAdmin,
    Path((key, id)): Path<(String, i64)>,
    Form(form): Form<RuleForm>,
) -> WebResult<Response> {
    let k = kind(&key)?;
    match validate_rule(k, &form) {
        Ok((a, b)) => {
            save_rule(&st.db, k, Some(id), &form, a, b).await?;
            Ok(see_other(&format!("/admin/rules/{}", k.key)))
        }
        Err(e) => Ok(page(edit_page(&me, k, Some(id), form, e, rule_meta(&st.db).await?))?.into_response()),
    }
}

// ---------------------------------------------------------------------------
// Persönliche Makros
// ---------------------------------------------------------------------------


fn personal_page(me: &User, id: Option<i64>, form: RuleForm, error: String, meta_json: String) -> AppResult<RuleEditPage> {
    let mut p = edit_page(me, kind("macros")?, id, form, error, meta_json);
    p.personal = true;
    p.nav = Nav::new(me, "me");
    p.action = match id {
        Some(id) => format!("/me/macros/{id}"),
        None => "/me/macros".into(),
    };
    Ok(p)
}

async fn my_macros(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Html<String>> {
    let rows: Vec<(i64, String, String, i32, bool)> = sqlx::query_as(
        "SELECT id, title, description, position, active FROM macros WHERE owner_id = $1 ORDER BY position, id",
    )
    .bind(me.id)
    .fetch_all(&st.db)
    .await?;
    page(RulesPage {
        nav: Nav::new(&me, "me"),
        tab: "macros",
        title: "Meine Makros",
        singular: "Makro",
        rules: rows
            .into_iter()
            .map(|(id, title, description, position, active)| RuleRow { id, title, description, position, active })
            .collect(),
        personal: true,
        base: "/me/macros".into(),
    })
}

async fn my_macro_new(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Html<String>> {
    let k = kind("macros")?;
    let form = RuleForm {
        title: String::new(),
        description: String::new(),
        position: 0,
        active: Some("on".into()),
        a: k.a_default.into(),
        b: None,
        ..Default::default()
    };
    page(personal_page(&me, None, form, String::new(), rule_meta(&st.db).await?)?)
}

async fn my_macro_edit(State(st): State<AppState>, WebAgent(me): WebAgent, Path(id): Path<i64>) -> WebResult<Html<String>> {
    let row: (String, String, i32, bool, Value) =
        sqlx::query_as("SELECT title, description, position, active, actions FROM macros WHERE id = $1 AND owner_id = $2")
            .bind(id)
            .bind(me.id)
            .fetch_optional(&st.db)
            .await?
            .ok_or(AppError::NotFound)?;
    let form = RuleForm {
        title: row.0,
        description: row.1,
        position: row.2,
        active: row.3.then(|| "on".into()),
        a: serde_json::to_string_pretty(&row.4).unwrap_or_default(),
        b: None,
        ..Default::default()
    };
    page(personal_page(&me, Some(id), form, String::new(), rule_meta(&st.db).await?)?)
}

async fn my_macro_create(State(st): State<AppState>, WebAgent(me): WebAgent, Form(form): Form<RuleForm>) -> WebResult<Response> {
    match validate_rule(kind("macros")?, &form) {
        Ok((actions, _)) => {
            sqlx::query("INSERT INTO macros (title, description, position, active, actions, owner_id) VALUES ($1, $2, $3, $4, $5, $6)")
                .bind(form.title.trim())
                .bind(form.description.trim())
                .bind(form.position)
                .bind(form.active.is_some())
                .bind(actions)
                .bind(me.id)
                .execute(&st.db)
                .await?;
            Ok(see_other("/me/macros"))
        }
        Err(e) => Ok(page(personal_page(&me, None, form, e, rule_meta(&st.db).await?)?)?.into_response()),
    }
}

async fn my_macro_update(
    State(st): State<AppState>,
    WebAgent(me): WebAgent,
    Path(id): Path<i64>,
    Form(form): Form<RuleForm>,
) -> WebResult<Response> {
    match validate_rule(kind("macros")?, &form) {
        Ok((actions, _)) => {
            let done = sqlx::query(
                "UPDATE macros SET title = $1, description = $2, position = $3, active = $4, actions = $5, updated_at = now()
                 WHERE id = $6 AND owner_id = $7",
            )
            .bind(form.title.trim())
            .bind(form.description.trim())
            .bind(form.position)
            .bind(form.active.is_some())
            .bind(actions)
            .bind(id)
            .bind(me.id)
            .execute(&st.db)
            .await?;
            if done.rows_affected() == 0 {
                return Err(AppError::NotFound.into());
            }
            Ok(see_other("/me/macros"))
        }
        Err(e) => Ok(page(personal_page(&me, Some(id), form, e, rule_meta(&st.db).await?)?)?.into_response()),
    }
}

async fn my_macro_delete(State(st): State<AppState>, WebAgent(me): WebAgent, Path(id): Path<i64>) -> WebResult<Response> {
    sqlx::query("DELETE FROM macros WHERE id = $1 AND owner_id = $2").bind(id).bind(me.id).execute(&st.db).await?;
    Ok(see_other("/me/macros"))
}

async fn delete_rule(
    State(st): State<AppState>,
    WebAdmin(_): WebAdmin,
    Path((key, id)): Path<(String, i64)>,
) -> WebResult<Response> {
    let k = kind(&key)?;
    sqlx::query(&format!("DELETE FROM {} WHERE id = $1", k.table)).bind(id).execute(&st.db).await?;
    Ok(see_other(&format!("/admin/rules/{}", k.key)))
}
