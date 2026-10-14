//! Wer sieht sich gerade welches Ticket an (Kollisionserkennung)?
//! Bewusst nur im Speicher: Die Angaben veralten nach wenigen Sekunden ohnehin.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

/// Ohne neues Lebenszeichen gilt ein Agent nach dieser Zeit als weg.
const TTL: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct Presence {
    tickets: Mutex<HashMap<i64, HashMap<i64, (Instant, bool)>>>,
}

impl Presence {
    /// Meldet `user` im Ticket an (optional „schreibt gerade“) und liefert die anderen Anwesenden.
    pub fn touch(&self, ticket: i64, user: i64, composing: bool) -> Vec<(i64, bool)> {
        let now = Instant::now();
        let mut map = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, viewers| {
            viewers.retain(|_, (seen, _)| now.duration_since(*seen) < TTL);
            !viewers.is_empty()
        });
        let viewers = map.entry(ticket).or_default();
        viewers.insert(user, (now, composing));
        viewers.iter().filter(|(uid, _)| **uid != user).map(|(uid, (_, c))| (*uid, *c)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Presence;

    #[test]
    fn others_only() {
        let p = Presence::default();
        assert!(p.touch(1, 10, false).is_empty());
        assert_eq!(p.touch(1, 11, true), vec![(10, false)]);
        assert_eq!(p.touch(1, 10, false), vec![(11, true)]);
        assert!(p.touch(2, 10, false).is_empty());
    }
}
