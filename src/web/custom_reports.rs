//! Eigene Berichte: Kennzahl × Gruppierung × Filter frei kombinieren, als Balken
//! und Tabelle anzeigen, als CSV exportieren und für alle Agenten speichern.
//!
//! Alle SQL-Bausteine stammen aus festen Listen (keine Nutzereingabe im SQL-Text);
//! Filterwerte werden gebunden.

use askama::Template;
use axum::{
    http::header,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use chrono::Local;
use serde::{Deserialize, Serialize};

use super::extract::{Form, Path, Query, State};
use super::*;
use crate::auth::WebAgent;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/reports/custom", get(show))
        .route("/reports/custom/save", post(save))
        .route("/reports/custom/{id}/delete", post(delete))
}

/// Kennzahlen: Schlüssel, Bezeichnung, Einheit, Datumsspalte, Wert-Ausdruck, Zusatzbedingung.
struct Metric {
    key: &'static str,
    label: &'static str,
    unit: &'static str,
    date: &'static str,
    value: &'static str,
    cond: &'static str,
}

const METRICS: [Metric; 7] = [
    Metric { key: "created", label: "Neue Tickets", unit: "", date: "t.created_at", value: "count(*)", cond: "true" },
    Metric { key: "solved", label: "Gelöste Tickets", unit: "", date: "t.solved_at", value: "count(*)", cond: "t.solved_at IS NOT NULL" },
    Metric {
        key: "first_reply",
        label: "Ø Erstantwortzeit",
        unit: "Std",
        date: "t.created_at",
        value: "avg(extract(epoch FROM t.first_reply_at - t.created_at)) / 3600",
        cond: "t.first_reply_at IS NOT NULL",
    },
    Metric {
        key: "resolution",
        label: "Ø Lösungszeit",
        unit: "Std",
        date: "t.solved_at",
        value: "avg(extract(epoch FROM t.solved_at - t.created_at)) / 3600",
        cond: "t.solved_at IS NOT NULL",
    },
    Metric {
        key: "csat",
        label: "Kundenzufriedenheit",
        unit: "%",
        date: "t.satisfaction_at",
        value: "100.0 * avg(CASE WHEN t.satisfaction_score = 'good' THEN 1 ELSE 0 END)",
        cond: "t.satisfaction_score IS NOT NULL",
    },
    Metric { key: "time", label: "Erfasste Arbeitszeit", unit: "Std", date: "e.created_at", value: "sum(e.seconds) / 3600.0", cond: "true" },
    Metric {
        key: "open_now",
        label: "Aktuell offene Tickets",
        unit: "",
        date: "",
        value: "count(*)",
        cond: "t.status IN ('new', 'open', 'pending', 'hold')",
    },
];

const GROUPS: [(&str, &str); 13] = [
    ("none", "Gesamt"),
    ("day", "Tag"),
    ("week", "Woche"),
    ("month", "Monat"),
    ("status", "Status"),
    ("priority", "Priorität"),
    ("channel", "Kanal"),
    ("type", "Typ"),
    ("group", "Gruppe"),
    ("assignee", "Bearbeiter"),
    ("organization", "Organisation"),
    ("tag", "Tag (Schlagwort)"),
    ("weekday", "Wochentag"),
];

const RANGES: [(i64, &str); 5] = [(7, "7 Tage"), (30, "30 Tage"), (90, "90 Tage"), (365, "12 Monate"), (3650, "Gesamter Zeitraum")];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportConfig {
    #[serde(default = "default_metric")]
    metric: String,
    #[serde(default = "default_group")]
    group_by: String,
    #[serde(default = "default_days")]
    days: i64,
    #[serde(default)]
    channel: String,
    #[serde(default)]
    priority: String,
    #[serde(default)]
    group_id: String,
    #[serde(default)]
    assignee_id: String,
    #[serde(default)]
    tag: String,
}

fn default_metric() -> String {
    "created".into()
}
fn default_group() -> String {
    "week".into()
}
fn default_days() -> i64 {
    90
}

impl ReportConfig {
    fn metric(&self) -> &'static Metric {
        METRICS.iter().find(|m| m.key == self.metric).unwrap_or(&METRICS[0])
    }

    fn group(&self) -> &'static str {
        GROUPS.iter().find(|(k, _)| *k == self.group_by).map(|(k, _)| *k).unwrap_or("none")
    }

    fn days(&self) -> i64 {
        RANGES.iter().find(|(d, _)| *d == self.days).map(|(d, _)| *d).unwrap_or(90)
    }

    fn query_string(&self) -> String {
        serde_urlencoded::to_string([
            ("metric", self.metric.as_str()),
            ("group_by", self.group_by.as_str()),
            ("days", &self.days.to_string()),
            ("channel", &self.channel),
            ("priority", &self.priority),
            ("group_id", &self.group_id),
            ("assignee_id", &self.assignee_id),
            ("tag", &self.tag),
        ])
        .unwrap_or_default()
    }
}

pub struct Row {
    pub label: String,
    pub value: f64,
    pub tickets: i64,
}

fn key_expr(group: &str, date: &str) -> String {
    let d = if date.is_empty() { "t.created_at" } else { date };
    match group {
        "day" => format!("to_char(date_trunc('day', {d}), 'YYYY-MM-DD')"),
        "week" => format!("to_char(date_trunc('week', {d}), 'IYYY \"KW\" IW')"),
        "month" => format!("to_char(date_trunc('month', {d}), 'YYYY-MM')"),
        "weekday" => format!("to_char({d}, 'ID')"),
        "status" => "t.status".into(),
        "priority" => "COALESCE(t.priority, '')".into(),
        "channel" => "t.channel".into(),
        "type" => "COALESCE(t.ticket_type, '')".into(),
        "group" => "COALESCE(g.name, '(keine Gruppe)')".into(),
        "assignee" => "COALESCE(a.name, '(nicht zugewiesen)')".into(),
        "organization" => "COALESCE(o.name, '(keine Organisation)')".into(),
        "tag" => "tg.tag".into(),
        _ => "'Gesamt'".into(),
    }
}

fn is_time_group(group: &str) -> bool {
    matches!(group, "day" | "week" | "month" | "weekday")
}

pub async fn run(db: &PgPool, cfg: &ReportConfig) -> AppResult<Vec<Row>> {
    let m = cfg.metric();
    let group = cfg.group();
    let from = if m.key == "time" { "time_entries e JOIN tickets t ON t.id = e.ticket_id" } else { "tickets t" };
    // Bei Zeiterfassung zählt, wer die Zeit gebucht hat.
    let assignee_col = if m.key == "time" { "e.user_id" } else { "t.assignee_id" };
    let tag_join = if group == "tag" {
        "CROSS JOIN LATERAL unnest(CASE WHEN cardinality(t.tags) = 0 THEN ARRAY['(ohne Tag)'] ELSE t.tags END) AS tg(tag)"
    } else {
        ""
    };
    let date_cond = if m.date.is_empty() { "true".to_string() } else { format!("{} >= now() - make_interval(days => $1::int)", m.date) };
    let order = if is_time_group(group) { "1 ASC" } else { "2 DESC NULLS LAST, 1" };
    let sql = format!(
        "SELECT {key} AS k, ({value})::float8 AS v, count(DISTINCT t.id) AS n
         FROM {from}
         LEFT JOIN agent_groups g ON g.id = t.group_id
         LEFT JOIN users a ON a.id = {assignee_col}
         LEFT JOIN organizations o ON o.id = t.organization_id
         {tag_join}
         WHERE {date_cond} AND {cond}
           AND ($2 = '' OR t.channel = $2)
           AND ($3 = '' OR t.priority = $3)
           AND ($4 = 0 OR t.group_id = $4)
           AND ($5 = 0 OR {assignee_col} = $5)
           AND ($6 = '' OR $6 = ANY(t.tags))
         GROUP BY 1 ORDER BY {order} LIMIT 100",
        key = key_expr(group, m.date),
        value = m.value,
        cond = m.cond,
    );
    let rows: Vec<(Option<String>, Option<f64>, i64)> = sqlx::query_as(&sql)
        .bind(cfg.days() as i32)
        .bind(&cfg.channel)
        .bind(&cfg.priority)
        .bind(cfg.group_id.parse::<i64>().unwrap_or(0))
        .bind(cfg.assignee_id.parse::<i64>().unwrap_or(0))
        .bind(cfg.tag.trim().to_lowercase())
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(k, v, n)| {
            let k = k.unwrap_or_default();
            let label = match group {
                "status" => status_label(&k).to_string(),
                "priority" if k.is_empty() => "(keine)".into(),
                "priority" => priority_label(&k).to_string(),
                "channel" => channel_label(&k).to_string(),
                "type" if k.is_empty() => "(kein Typ)".into(),
                "type" => type_label(&k).to_string(),
                "weekday" => ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"]
                    .get(k.parse::<usize>().unwrap_or(1).saturating_sub(1))
                    .unwrap_or(&"?")
                    .to_string(),
                _ => k,
            };
            Row { label, value: v.unwrap_or(0.0), tickets: n }
        })
        .collect())
}

fn fmt_value(v: f64, unit: &str) -> String {
    match unit {
        "" => format!("{}", v.round() as i64),
        "%" => format!("{} %", v.round() as i64),
        u => format!("{} {u}", format!("{v:.1}").replace('.', ",")),
    }
}

// ---------------------------------------------------------------------------
// Seite
// ---------------------------------------------------------------------------

struct Bar {
    label: String,
    value: String,
    tickets: i64,
    pct: u32,
}

struct Saved {
    id: i64,
    name: String,
    url: String,
    by: String,
    can_delete: bool,
}

#[derive(Template)]
#[template(path = "custom_report.html")]
struct ReportPage {
    nav: Nav,
    title: String,
    metric_opts: Vec<Opt>,
    group_opts: Vec<Opt>,
    range_opts: Vec<Opt>,
    channel_opts: Vec<Opt>,
    priority_opts: Vec<Opt>,
    group_id_opts: Vec<Opt>,
    assignee_opts: Vec<Opt>,
    tag: String,
    bars: Vec<Bar>,
    total: String,
    unit_hint: &'static str,
    count_label: &'static str,
    query: String,
    saved: Vec<Saved>,
    cfg_json: String,
    notice: String,
}

#[derive(Deserialize)]
struct ShowQuery {
    report: Option<i64>,
    format: Option<String>,
    notice: Option<String>,
    metric: Option<String>,
    group_by: Option<String>,
    days: Option<String>,
    channel: Option<String>,
    priority: Option<String>,
    group_id: Option<String>,
    assignee_id: Option<String>,
    tag: Option<String>,
}

impl ShowQuery {
    fn config(&self) -> ReportConfig {
        let s = |v: &Option<String>| v.clone().unwrap_or_default();
        ReportConfig {
            metric: self.metric.clone().unwrap_or_else(default_metric),
            group_by: self.group_by.clone().unwrap_or_else(default_group),
            days: self.days.as_deref().and_then(|d| d.parse().ok()).unwrap_or_else(default_days),
            channel: s(&self.channel),
            priority: s(&self.priority),
            group_id: s(&self.group_id),
            assignee_id: s(&self.assignee_id),
            tag: s(&self.tag),
        }
    }
}

fn opts(items: impl IntoIterator<Item = (String, String)>, selected: &str) -> Vec<Opt> {
    items.into_iter().map(|(value, label)| Opt { selected: value == selected, value, label }).collect()
}

async fn show(State(st): State<AppState>, WebAgent(me): WebAgent, Query(q): Query<ShowQuery>) -> WebResult<Response> {
    let db = &st.db;
    let mut title = String::new();
    let cfg: ReportConfig = match q.report {
        Some(id) => {
            let (name, cfg): (String, sqlx::types::Json<ReportConfig>) =
                sqlx::query_as("SELECT name, config FROM saved_reports WHERE id = $1")
                    .bind(id)
                    .fetch_optional(db)
                    .await?
                    .ok_or(AppError::NotFound)?;
            title = name;
            cfg.0
        }
        None => q.config(),
    };
    let m = cfg.metric();
    let rows = run(db, &cfg).await?;

    if q.format.as_deref() == Some("csv") {
        let group_label = GROUPS.iter().find(|(k, _)| *k == cfg.group()).map(|(_, l)| *l).unwrap_or("");
        let mut csv = format!("\u{feff}{group_label};{} {};Tickets\r\n", m.label, m.unit);
        for r in &rows {
            csv.push_str(&format!(
                "{};{};{}\r\n",
                r.label.replace(';', ","),
                format!("{:.2}", r.value).replace('.', ","),
                r.tickets
            ));
        }
        let filename = format!("bericht-{}-{}.csv", m.key, Local::now().format("%Y-%m-%d"));
        return Ok((
            [
                (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\"")),
            ],
            csv,
        )
            .into_response());
    }

    let max = rows.iter().map(|r| r.value).fold(0.0_f64, f64::max);
    let bars: Vec<Bar> = rows
        .iter()
        .map(|r| Bar {
            label: r.label.clone(),
            value: fmt_value(r.value, m.unit),
            tickets: r.tickets,
            pct: if max > 0.0 { ((r.value / max) * 100.0).round().clamp(0.0, 100.0) as u32 } else { 0 },
        })
        .collect();
    // Summe nur bei additiven Kennzahlen sinnvoll; bei Durchschnitten nicht.
    let total = match m.key {
        "created" | "solved" | "open_now" | "time" if cfg.group() != "tag" => {
            fmt_value(rows.iter().map(|r| r.value).sum(), m.unit)
        }
        _ => String::new(),
    };

    let groups: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM agent_groups ORDER BY name").fetch_all(db).await?;
    let agents: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, name FROM users WHERE role IN ('agent', 'admin') AND email <> $1 ORDER BY name",
    )
    .bind(crate::automations::SYSTEM_EMAIL)
    .fetch_all(db)
    .await?;
    #[allow(clippy::type_complexity)]
    let saved: Vec<(i64, String, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT r.id, r.name, r.created_by, u.name FROM saved_reports r LEFT JOIN users u ON u.id = r.created_by ORDER BY r.name",
    )
    .fetch_all(db)
    .await?;
    let all = |label: &str| (String::new(), label.to_string());

    let html = page(ReportPage {
        nav: Nav::new(&me, "reports"),
        title,
        metric_opts: opts(METRICS.iter().map(|m| (m.key.to_string(), m.label.to_string())), m.key),
        group_opts: opts(GROUPS.iter().map(|(k, l)| (k.to_string(), l.to_string())), cfg.group()),
        range_opts: opts(RANGES.iter().map(|(d, l)| (d.to_string(), l.to_string())), &cfg.days().to_string()),
        channel_opts: opts(
            std::iter::once(all("alle Kanäle")).chain(CHANNELS.iter().map(|c| (c.to_string(), channel_label(c).to_string()))),
            &cfg.channel,
        ),
        priority_opts: opts(
            std::iter::once(all("alle Prioritäten")).chain(PRIORITIES.iter().map(|p| (p.to_string(), priority_label(p).to_string()))),
            &cfg.priority,
        ),
        group_id_opts: opts(
            std::iter::once(all("alle Gruppen")).chain(groups.into_iter().map(|(id, n)| (id.to_string(), n))),
            &cfg.group_id,
        ),
        assignee_opts: opts(
            std::iter::once(all("alle Agenten")).chain(agents.into_iter().map(|(id, n)| (id.to_string(), n))),
            &cfg.assignee_id,
        ),
        tag: cfg.tag.clone(),
        bars,
        total,
        unit_hint: match m.unit {
            "Std" => "in Stunden",
            "%" => "Anteil „gut“ an allen Bewertungen",
            _ => "",
        },
        count_label: if m.key == "time" { "Tickets mit Zeit" } else { "Tickets" },
        query: cfg.query_string(),
        saved: saved
            .into_iter()
            .map(|(id, name, by_id, by)| Saved {
                id,
                url: format!("/reports/custom?report={id}"),
                name,
                by: by.unwrap_or_default(),
                can_delete: me.is_admin() || by_id == Some(me.id),
            })
            .collect(),
        cfg_json: serde_json::to_string(&cfg).unwrap_or_default(),
        notice: q.notice.unwrap_or_default(),
    })?;
    Ok(html.into_response())
}

#[derive(Deserialize)]
struct SaveForm {
    name: String,
    config: String,
}

async fn save(State(st): State<AppState>, WebAgent(me): WebAgent, Form(f): Form<SaveForm>) -> WebResult<Response> {
    let name = f.name.trim();
    if name.is_empty() {
        return Err(AppError::invalid("Bitte einen Namen für den Bericht angeben").into());
    }
    let cfg: ReportConfig = serde_json::from_str(&f.config).map_err(|_| AppError::invalid("Ungültige Berichtseinstellungen"))?;
    let id: i64 = sqlx::query_scalar("INSERT INTO saved_reports (name, config, created_by) VALUES ($1, $2, $3) RETURNING id")
        .bind(name.chars().take(120).collect::<String>())
        .bind(sqlx::types::Json(&cfg))
        .bind(me.id)
        .fetch_one(&st.db)
        .await?;
    Ok(see_other(&format!("/reports/custom?report={id}")))
}

async fn delete(State(st): State<AppState>, WebAgent(me): WebAgent, Path(id): Path<i64>) -> WebResult<Response> {
    let done = sqlx::query("DELETE FROM saved_reports WHERE id = $1 AND ($2 OR created_by = $3)")
        .bind(id)
        .bind(me.is_admin())
        .bind(me.id)
        .execute(&st.db)
        .await?;
    if done.rows_affected() == 0 {
        return Err(AppError::Forbidden.into());
    }
    Ok(see_other("/reports/custom?notice=Bericht+gel%C3%B6scht"))
}
