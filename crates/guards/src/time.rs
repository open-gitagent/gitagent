//! Time budget (BLUEPRINT §5 step 7): a reminder at `nudge_at` of the budget
//! and a firm "finish now" at `force_stop_at`. Timeouts count as failures, so
//! a finished, verified partial result beats running out of time.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeEvent {
    Nudge,
    ForceStop,
}

pub struct TimeBudget {
    start: Instant,
    total: Duration,
    nudge_at: f32,
    force_stop_at: f32,
    nudged: bool,
    forced: bool,
}

impl TimeBudget {
    pub fn new(start: Instant, total: Duration, nudge_at: f32, force_stop_at: f32) -> Self {
        TimeBudget {
            start,
            total,
            nudge_at,
            force_stop_at,
            nudged: false,
            forced: false,
        }
    }

    pub fn remaining(&self, now: Instant) -> Duration {
        self.total
            .saturating_sub(now.saturating_duration_since(self.start))
    }

    /// Whole minutes left, rounded down.
    pub fn minutes_left(&self, now: Instant) -> u64 {
        self.remaining(now).as_secs() / 60
    }

    pub fn forced(&self) -> bool {
        self.forced
    }

    /// Returns each event once, the first time its threshold is passed.
    pub fn check(&mut self, now: Instant) -> Option<TimeEvent> {
        if self.total.is_zero() {
            return None;
        }
        let used =
            now.saturating_duration_since(self.start).as_secs_f32() / self.total.as_secs_f32();
        if !self.forced && used >= self.force_stop_at {
            self.forced = true;
            self.nudged = true;
            return Some(TimeEvent::ForceStop);
        }
        if !self.nudged && used >= self.nudge_at {
            self.nudged = true;
            return Some(TimeEvent::Nudge);
        }
        None
    }

    /// The message to show the agent for an event.
    pub fn message(&self, event: TimeEvent, now: Instant) -> String {
        let left = self.minutes_left(now);
        let total = self.total.as_secs() / 60;
        match event {
            TimeEvent::Nudge => format!(
                "Time check: about {left} of {total} minutes left. Focus on finishing the core requirements and verifying them; skip optional polish."
            ),
            TimeEvent::ForceStop => format!(
                "Time is almost up: about {left} minutes left. Stop exploring. Make sure what you have works, verify it once, and call task_complete now."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_nudge_then_force_once_each() {
        let start = Instant::now();
        let mut t = TimeBudget::new(start, Duration::from_secs(1000), 0.7, 0.9);
        assert_eq!(t.check(start + Duration::from_secs(100)), None);
        assert_eq!(
            t.check(start + Duration::from_secs(700)),
            Some(TimeEvent::Nudge)
        );
        assert_eq!(t.check(start + Duration::from_secs(750)), None);
        assert_eq!(
            t.check(start + Duration::from_secs(900)),
            Some(TimeEvent::ForceStop)
        );
        assert!(t.forced());
        assert_eq!(t.check(start + Duration::from_secs(950)), None);
    }

    #[test]
    fn jumping_past_both_thresholds_forces_without_nudging() {
        let start = Instant::now();
        let mut t = TimeBudget::new(start, Duration::from_secs(600), 0.7, 0.9);
        assert_eq!(
            t.check(start + Duration::from_secs(590)),
            Some(TimeEvent::ForceStop)
        );
        assert_eq!(t.check(start + Duration::from_secs(595)), None);
    }

    #[test]
    fn reports_minutes_left() {
        let start = Instant::now();
        let t = TimeBudget::new(start, Duration::from_secs(1800), 0.7, 0.9);
        assert_eq!(t.minutes_left(start + Duration::from_secs(1260)), 9);
        assert!(t
            .message(TimeEvent::Nudge, start + Duration::from_secs(1260))
            .starts_with("Time check: about 9 of 30 minutes left."));
    }
}
