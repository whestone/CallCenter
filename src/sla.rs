//! SLA-Richtlinien: Zielzeiten je Priorität für Erstantwort und Lösung.

use std::{collections::HashSet, sync::Arc};

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::models::{SlaPolicy, Ticket};

pub const METRICS: [&str; 2] = ["first_reply_time", "full_resolution_time"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlaMetric {
    pub priority: String,
    pub metric: String,
    /// Zielzeit in Minuten.
    pub target: i64,
    /// Zielzeit zählt nur innerhalb der Geschäftszeiten (sonst Kalenderzeit).
    #[serde(default)]
    pub business_hours: bool,
}

/// Geschäftszeiten je Wochentag (Index 0 = Montag) und Feiertage, in lokaler Zeit.
#[derive(Debug, Clone, Default)]
pub struct Schedule {
    pub days: [Option<(NaiveTime, NaiveTime)>; 7],
    pub holidays: HashSet<NaiveDate>,
}

impl Schedule {
    fn window(&self, date: NaiveDate) -> Option<(NaiveTime, NaiveTime)> {
        if self.holidays.contains(&date) {
            return None;
        }
        self.days[date.weekday().num_days_from_monday() as usize]
    }

    /// Addiert Minuten Arbeitszeit ab `start`. Ohne konfigurierte Arbeitstage: Kalenderzeit.
    pub fn add_business_minutes(&self, start: DateTime<Utc>, minutes: i64) -> DateTime<Utc> {
        if self.days.iter().all(Option::is_none) {
            return start + Duration::minutes(minutes);
        }
        let mut cursor = start.with_timezone(&Local).naive_local();
        let mut remaining = Duration::minutes(minutes.max(0));
        for _ in 0..3660 {
            let date = cursor.date();
            if let Some((open, close)) = self.window(date) {
                let open_at = date.and_time(open);
                let close_at = date.and_time(close);
                if cursor < open_at {
                    cursor = open_at;
                }
                if cursor < close_at {
                    let available = close_at - cursor;
                    if remaining <= available {
                        let end = cursor + remaining;
                        return Local.from_local_datetime(&end).earliest().map(|d| d.with_timezone(&Utc)).unwrap_or(start);
                    }
                    remaining -= available;
                }
            }
            cursor = (date + Duration::days(1)).and_time(NaiveTime::MIN);
        }
        start + Duration::minutes(minutes)
    }
}

pub async fn load_schedule(ex: &sqlx::PgPool) -> sqlx::Result<Schedule> {
    let mut schedule = Schedule::default();
    let rows: Vec<(i32, bool, NaiveTime, NaiveTime)> =
        sqlx::query_as("SELECT weekday, enabled, opens_at, closes_at FROM business_hours").fetch_all(ex).await?;
    for (weekday, enabled, opens, closes) in rows {
        if enabled && (1..=7).contains(&weekday) {
            schedule.days[(weekday - 1) as usize] = Some((opens, closes));
        }
    }
    let days: Vec<NaiveDate> = sqlx::query_scalar("SELECT day FROM holidays").fetch_all(ex).await?;
    schedule.holidays = days.into_iter().collect();
    Ok(schedule)
}

#[derive(Debug, Clone, Serialize)]
pub struct SlaTarget {
    pub metric: String,
    pub label: &'static str,
    pub due_at: DateTime<Utc>,
    pub fulfilled: bool,
    pub breached: bool,
    pub minutes_left: i64,
    /// Die Uhr steht gerade (Ticket in einem pausierenden Status).
    pub paused: bool,
}

pub fn metric_label(m: &str) -> &'static str {
    match m {
        "first_reply_time" => "Erste Antwort",
        "full_resolution_time" => "Lösung",
        _ => "?",
    }
}

pub fn validate_metrics(metrics: &[SlaMetric]) -> Result<(), String> {
    for m in metrics {
        if !crate::models::PRIORITIES.contains(&m.priority.as_str()) {
            return Err(format!("Ungültige Priorität im SLA: {}", m.priority));
        }
        if !METRICS.contains(&m.metric.as_str()) {
            return Err(format!("Unbekannte SLA-Metrik: {}", m.metric));
        }
        if m.target <= 0 {
            return Err("SLA-Zielzeit muss positiv sein".into());
        }
    }
    Ok(())
}

pub fn evaluate(policy: &SlaPolicy, t: &Ticket, now: DateTime<Utc>) -> Vec<SlaTarget> {
    let priority = t.priority.as_deref().unwrap_or("normal");
    // Pausen (z. B. „Wartend“) verschieben die Fälligkeit um die angehaltene Zeit.
    let paused_now = t.sla_paused_since.is_some();
    let paused = Duration::seconds(t.sla_paused_seconds) + t.sla_paused_since.map(|s| now - s).unwrap_or_else(Duration::zero);
    policy
        .metrics
        .0
        .iter()
        .filter(|m| m.priority == priority)
        .filter_map(|m| {
            let done_at = match m.metric.as_str() {
                // Ein ohne Antwort gelöstes Ticket gilt als beantwortet.
                "first_reply_time" => t.first_reply_at.or(t.solved_at),
                "full_resolution_time" => t.solved_at,
                _ => return None,
            };
            let due_at = match (&policy.schedule, m.business_hours) {
                (Some(schedule), true) => schedule.add_business_minutes(t.created_at, m.target),
                _ => t.created_at + Duration::minutes(m.target),
            } + paused;
            Some(SlaTarget {
                metric: m.metric.clone(),
                label: metric_label(&m.metric),
                due_at,
                fulfilled: done_at.is_some(),
                breached: done_at.unwrap_or(now) > due_at,
                minutes_left: (due_at - now).num_minutes(),
                paused: paused_now && done_at.is_none(),
            })
        })
        .collect()
}

/// Das nächste offene SLA-Ziel (für die Ticketliste).
pub fn next_target(policy: &SlaPolicy, t: &Ticket, now: DateTime<Utc>) -> Option<SlaTarget> {
    if t.status == "closed" || t.status == "solved" {
        return None;
    }
    evaluate(policy, t, now)
        .into_iter()
        .filter(|s| !s.fulfilled)
        .min_by_key(|s| s.due_at)
}

/// Richtlinien ohne Geschäftszeiten (reicht für die Zuordnung per Filter).
pub async fn load_policies<'e, E>(ex: E) -> sqlx::Result<Vec<SlaPolicy>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_as("SELECT * FROM sla_policies WHERE active ORDER BY position, id")
        .fetch_all(ex)
        .await
}

/// Richtlinien inklusive Geschäftszeiten (für Fälligkeiten in Anzeige und Reports).
pub async fn load_policies_with_schedule(db: &sqlx::PgPool) -> sqlx::Result<Vec<SlaPolicy>> {
    let schedule = Arc::new(load_schedule(db).await?);
    let mut policies = load_policies(db).await?;
    for p in &mut policies {
        p.schedule = Some(schedule.clone());
    }
    Ok(policies)
}

pub async fn load_policy(db: &sqlx::PgPool, id: Option<i64>) -> sqlx::Result<Option<SlaPolicy>> {
    let Some(id) = id else { return Ok(None) };
    let policy: Option<SlaPolicy> = sqlx::query_as("SELECT * FROM sla_policies WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?;
    match policy {
        Some(mut p) => {
            p.schedule = Some(Arc::new(load_schedule(db).await?));
            Ok(Some(p))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule() -> Schedule {
        let nine_to_six = Some((NaiveTime::from_hms_opt(8, 0, 0).unwrap(), NaiveTime::from_hms_opt(18, 0, 0).unwrap()));
        Schedule { days: [nine_to_six, nine_to_six, nine_to_six, nine_to_six, nine_to_six, None, None], holidays: HashSet::new() }
    }

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn business_minutes() {
        let s = schedule();
        // Innerhalb eines Tages
        assert_eq!(s.add_business_minutes(at(2026, 10, 14, 9, 0), 60), at(2026, 10, 14, 10, 0));
        // Freitag 17:00 + 2 Std → Montag 09:00
        assert_eq!(s.add_business_minutes(at(2026, 10, 16, 17, 0), 120), at(2026, 10, 19, 9, 0));
        // Nachts eingegangen → zählt ab 08:00
        assert_eq!(s.add_business_minutes(at(2026, 10, 14, 2, 0), 30), at(2026, 10, 14, 8, 30));
        // Feiertag wird übersprungen: Do 24.12. 17:00 + 2 Std, 25.12. (Fr) frei → Mo 28.12. 09:00
        let mut s = schedule();
        s.holidays.insert(NaiveDate::from_ymd_opt(2026, 12, 25).unwrap());
        assert_eq!(s.add_business_minutes(at(2026, 12, 24, 17, 0), 120), at(2026, 12, 28, 9, 0));
        // Keine Arbeitstage konfiguriert → Kalenderzeit
        assert_eq!(Schedule::default().add_business_minutes(at(2026, 10, 16, 17, 0), 120), at(2026, 10, 16, 19, 0));
    }
}

pub fn fmt_minutes(minutes: i64) -> String {
    let sign = if minutes < 0 { "-" } else { "" };
    let m = minutes.abs();
    if m < 60 {
        format!("{sign}{m} Min")
    } else if m < 60 * 24 {
        format!("{sign}{} Std", m / 60)
    } else {
        format!("{sign}{} T", m / (60 * 24))
    }
}
