//! Reporting: Kennzahlen, Verlauf, Verteilungen, Agenten-Statistik, CSV-Export.

use std::collections::{BTreeMap, HashMap};

use askama::Template;
use axum::{
    http::header,
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use serde::Deserialize;

use super::extract::{Query, State};
use super::*;
use crate::auth::WebAgent;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/reports", get(reports))
        .route("/reports/export.csv", get(export_csv))
}

#[derive(Deserialize)]
struct RangeQuery {
    days: Option<String>,
}

fn range_days(q: &RangeQuery) -> i64 {
    match q.days.as_deref().and_then(|d| d.parse::<i64>().ok()) {
        Some(d @ (7 | 30 | 90 | 365)) => d,
        _ => 30,
    }
}

struct Tile {
    label: &'static str,
    value: String,
    hint: String,
}

struct DayBar {
    label: String,
    tip: String,
    created: i64,
    solved: i64,
    created_pct: u32,
    solved_pct: u32,
    show_label: bool,
}

struct ShareBar {
    label: String,
    count: i64,
    pct: u32,
}

struct AgentStat {
    name: String,
    open: i64,
    solved: i64,
    avg_first_reply: String,
}

#[derive(Template)]
#[template(path = "reports.html")]
struct ReportsPage {
    nav: Nav,
    days: i64,
    range_opts: Vec<Opt>,
    tiles: Vec<Tile>,
    day_bars: Vec<DayBar>,
    max_per_day: i64,
    channels: Vec<ShareBar>,
    priorities: Vec<ShareBar>,
    agents: Vec<AgentStat>,
}

fn fmt_duration(minutes: Option<f64>) -> String {
    match minutes {
        None => "–".into(),
        Some(m) if m < 60.0 => format!("{} Min", m.round() as i64),
        Some(m) if m < 60.0 * 24.0 => format!("{:.1} Std", m / 60.0).replace('.', ","),
        Some(m) => format!("{:.1} T", m / 1440.0).replace('.', ","),
    }
}

fn avg(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / n as f64)
}

fn minutes_between(a: DateTime<Utc>, b: DateTime<Utc>) -> f64 {
    (b - a).num_seconds() as f64 / 60.0
}

fn pct(part: i64, max: i64) -> u32 {
    if max <= 0 { 0 } else { ((part as f64 / max as f64) * 100.0).round() as u32 }
}

fn shares(counts: BTreeMap<String, i64>) -> Vec<ShareBar> {
    let max = counts.values().copied().max().unwrap_or(0);
    let mut bars: Vec<ShareBar> = counts
        .into_iter()
        .map(|(label, count)| ShareBar { pct: pct(count, max), label, count })
        .collect();
    bars.sort_by(|a, b| b.count.cmp(&a.count));
    bars
}

async fn reports(State(st): State<AppState>, WebAgent(me): WebAgent, Query(q): Query<RangeQuery>) -> WebResult<Html<String>> {
    let db = &st.db;
    let days = range_days(&q);
    let now = Utc::now();
    let from = now - Duration::days(days);

    let tickets: Vec<Ticket> = sqlx::query_as("SELECT * FROM tickets WHERE created_at >= $1 OR solved_at >= $1")
        .bind(from)
        .fetch_all(db)
        .await?;
    let created: Vec<&Ticket> = tickets.iter().filter(|t| t.created_at >= from).collect();
    let solved: Vec<&Ticket> = tickets.iter().filter(|t| t.solved_at.is_some_and(|s| s >= from)).collect();
    let backlog: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets WHERE status IN ('new', 'open', 'pending', 'hold')")
        .fetch_one(db)
        .await?;

    let first_reply = avg(created.iter().filter_map(|t| t.first_reply_at.map(|r| minutes_between(t.created_at, r))));
    let resolution = avg(solved.iter().filter_map(|t| t.solved_at.map(|s| minutes_between(t.created_at, s))));

    // SLA-Quote: Anteil der abgeschlossenen bzw. bereits verletzten Ziele, die eingehalten wurden.
    let policies: HashMap<i64, SlaPolicy> = sla::load_policies_with_schedule(db).await?.into_iter().map(|p| (p.id, p)).collect();
    let (mut sla_met, mut sla_total) = (0i64, 0i64);
    for t in &created {
        if let Some(p) = t.sla_policy_id.and_then(|id| policies.get(&id)) {
            for target in sla::evaluate(p, t, now) {
                if target.fulfilled || target.breached {
                    sla_total += 1;
                    if !target.breached {
                        sla_met += 1;
                    }
                }
            }
        }
    }

    let (csat_good, csat_total): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE satisfaction_score = 'good'), count(*)
         FROM tickets WHERE satisfaction_at >= $1",
    )
    .bind(from)
    .fetch_one(db)
    .await?;

    let tiles = vec![
        Tile { label: "Neue Tickets", value: created.len().to_string(), hint: format!("in {days} Tagen") },
        Tile { label: "Gelöst", value: solved.len().to_string(), hint: format!("in {days} Tagen") },
        Tile { label: "Offen (Backlog)", value: backlog.to_string(), hint: "aktuell ungelöst".into() },
        Tile { label: "Ø Erstantwort", value: fmt_duration(first_reply), hint: "neue Tickets im Zeitraum".into() },
        Tile { label: "Ø Lösungszeit", value: fmt_duration(resolution), hint: "gelöste Tickets im Zeitraum".into() },
        Tile {
            label: "SLA eingehalten",
            value: if sla_total == 0 { "–".into() } else { format!("{} %", pct(sla_met, sla_total)) },
            hint: format!("{sla_met} von {sla_total} Zielen"),
        },
        Tile {
            label: "Kundenzufriedenheit",
            value: if csat_total == 0 { "–".into() } else { format!("{} %", pct(csat_good, csat_total)) },
            hint: format!("{csat_good} von {csat_total} Bewertungen positiv"),
        },
    ];

    // Verlauf pro Tag (lokale Zeit), auch Tage ohne Tickets.
    let today = Local::now().date_naive();
    let mut per_day: BTreeMap<NaiveDate, (i64, i64)> =
        (0..days).map(|i| (today - Duration::days(days - 1 - i), (0, 0))).collect();
    for t in &created {
        if let Some(e) = per_day.get_mut(&t.created_at.with_timezone(&Local).date_naive()) {
            e.0 += 1;
        }
    }
    for t in &solved {
        if let Some(e) = t.solved_at.and_then(|s| per_day.get_mut(&s.with_timezone(&Local).date_naive())) {
            e.1 += 1;
        }
    }
    let max_per_day = per_day.values().map(|(c, s)| (*c).max(*s)).max().unwrap_or(0).max(1);
    let label_every = (days / 7).max(1) as usize;
    let count = per_day.len();
    let day_bars = per_day
        .into_iter()
        .enumerate()
        .map(|(i, (d, (c, s)))| DayBar {
            label: d.format("%d.%m.").to_string(),
            tip: format!("{}: {c} neu, {s} gelöst", d.format("%a %d.%m.")),
            created: c,
            solved: s,
            created_pct: pct(c, max_per_day),
            solved_pct: pct(s, max_per_day),
            show_label: (count - 1 - i) % label_every == 0,
        })
        .collect();

    let mut by_channel = BTreeMap::new();
    let mut by_priority = BTreeMap::new();
    for t in &created {
        *by_channel.entry(channel_label(&t.channel).to_string()).or_insert(0) += 1;
        *by_priority
            .entry(t.priority.as_deref().map(priority_label).unwrap_or("ohne Priorität").to_string())
            .or_insert(0) += 1;
    }

    // Agenten: aktuell offene, im Zeitraum gelöste Tickets, Ø Erstantwort ihrer Tickets.
    let agents: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT u.id, u.name,
                (SELECT count(*) FROM tickets t WHERE t.assignee_id = u.id AND t.status IN ('new', 'open', 'pending', 'hold'))
         FROM users u WHERE u.role IN ('agent', 'admin') AND u.active ORDER BY u.name",
    )
    .fetch_all(db)
    .await?;
    let agents = agents
        .into_iter()
        .map(|(id, name, open)| AgentStat {
            name,
            open,
            solved: solved.iter().filter(|t| t.assignee_id == Some(id)).count() as i64,
            avg_first_reply: fmt_duration(avg(created
                .iter()
                .filter(|t| t.assignee_id == Some(id))
                .filter_map(|t| t.first_reply_at.map(|r| minutes_between(t.created_at, r))))),
        })
        .collect();

    page(ReportsPage {
        nav: Nav::new(&me, "reports"),
        days,
        range_opts: [(7, "Letzte 7 Tage"), (30, "Letzte 30 Tage"), (90, "Letzte 90 Tage"), (365, "Letztes Jahr")]
            .into_iter()
            .map(|(d, l)| Opt { value: d.to_string(), label: l.into(), selected: d == days })
            .collect(),
        tiles,
        day_bars,
        max_per_day,
        channels: shares(by_channel),
        priorities: shares(by_priority),
        agents,
    })
}

fn csv_field(s: &str) -> String {
    // Formel-Injection in Excel verhindern und Sonderzeichen quoten.
    let s = if s.starts_with(['=', '+', '-', '@']) { format!("'{s}") } else { s.to_string() };
    if s.contains([';', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

async fn export_csv(State(st): State<AppState>, WebAgent(_): WebAgent, Query(q): Query<RangeQuery>) -> WebResult<Response> {
    let days = range_days(&q);
    let from = Utc::now() - Duration::days(days);
    #[allow(clippy::type_complexity)]
    let rows: Vec<(i64, String, String, Option<String>, Option<String>, String, String, Option<String>, Option<String>, Option<String>, Option<String>, Vec<String>, DateTime<Utc>, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> =
        sqlx::query_as(
            "SELECT t.id, t.subject, t.status, t.priority, t.ticket_type, t.channel, r.name, r.email, o.name, a.name, g.name,
                    t.tags, t.created_at, t.first_reply_at, t.solved_at
             FROM tickets t
             JOIN users r ON r.id = t.requester_id
             LEFT JOIN organizations o ON o.id = t.organization_id
             LEFT JOIN users a ON a.id = t.assignee_id
             LEFT JOIN agent_groups g ON g.id = t.group_id
             WHERE t.created_at >= $1 ORDER BY t.id",
        )
        .bind(from)
        .fetch_all(&st.db)
        .await?;

    // Semikolon + BOM: öffnet sich in deutschem Excel direkt korrekt.
    let mut out = String::from("\u{feff}ID;Betreff;Status;Priorität;Typ;Kanal;Anfragender;E-Mail;Organisation;Bearbeiter;Gruppe;Tags;Erstellt;Erste Antwort;Gelöst;Erstantwort (Min);Lösungszeit (Min)\r\n");
    let dt = |d: Option<DateTime<Utc>>| d.map(fmt_dt).unwrap_or_default();
    for (id, subject, status, priority, ty, channel, requester, email, org, assignee, group, tags, created, first, solved) in rows {
        let mins = |end: Option<DateTime<Utc>>| end.map(|e| (minutes_between(created, e).round() as i64).to_string()).unwrap_or_default();
        let fields = [
            id.to_string(),
            subject,
            status_label(&status).into(),
            priority.as_deref().map(priority_label).unwrap_or("").into(),
            ty.as_deref().map(type_label).unwrap_or("").into(),
            channel_label(&channel).into(),
            requester,
            email.unwrap_or_default(),
            org.unwrap_or_default(),
            assignee.unwrap_or_default(),
            group.unwrap_or_default(),
            tags.join(" "),
            fmt_dt(created),
            dt(first),
            dt(solved),
            mins(first),
            mins(solved),
        ];
        out.push_str(&fields.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(";"));
        out.push_str("\r\n");
    }
    let filename = format!("tickets-{}-tage-{}.csv", days, Local::now().format("%Y-%m-%d"));
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\"")),
        ],
        out,
    )
        .into_response())
}
