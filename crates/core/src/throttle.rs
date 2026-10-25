//! The adaptive window for model calls made at the same time (sub-agents,
//! plan tasks). It starts at the ceiling, `max_parallel`, halves whenever
//! the provider refuses a call for rate limits, and grows back one slot at a
//! time after a run of calls that went through. One throttle is shared by
//! every run of a session, because the limit is the account's, not a task's.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Calls that must go through in a row before a slot comes back.
const RECOVER_AFTER: u64 = 10;

#[derive(Debug)]
pub struct Throttle {
    ceiling: usize,
    window: AtomicUsize,
    clean: AtomicU64,
    hits: AtomicU64,
}

impl Throttle {
    pub fn new(ceiling: usize) -> Self {
        let ceiling = ceiling.max(1);
        Throttle {
            ceiling,
            window: AtomicUsize::new(ceiling),
            clean: AtomicU64::new(0),
            hits: AtomicU64::new(0),
        }
    }

    /// How many calls may run at once right now.
    pub fn window(&self) -> usize {
        self.window.load(Ordering::Relaxed)
    }

    /// The most that ever run at once (`max_parallel`).
    pub fn ceiling(&self) -> usize {
        self.ceiling
    }

    /// Rate-limit refusals so far.
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// A call was refused for rate limits: the window halves, to at least one.
    pub fn hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
        self.clean.store(0, Ordering::Relaxed);
        self.update(|w| (w / 2).max(1));
    }

    /// Replaces the window with `f` of its current value.
    fn update(&self, f: impl Fn(usize) -> usize) {
        let mut current = self.window.load(Ordering::Relaxed);
        loop {
            match self.window.compare_exchange_weak(
                current,
                f(current),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(now) => current = now,
            }
        }
    }

    /// A call went through: after enough in a row, one slot comes back.
    pub fn clean(&self) {
        if self.window() >= self.ceiling {
            self.clean.store(0, Ordering::Relaxed);
            return;
        }
        if self.clean.fetch_add(1, Ordering::Relaxed) + 1 >= RECOVER_AFTER {
            self.clean.store(0, Ordering::Relaxed);
            let ceiling = self.ceiling;
            self.update(|w| (w + 1).min(ceiling));
        }
    }

    /// (window, ceiling, hits), for events and the task board.
    pub fn state(&self) -> (usize, usize, u64) {
        (self.window(), self.ceiling, self.hits())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves_on_a_hit_and_recovers_one_slot_at_a_time() {
        let t = Throttle::new(8);
        assert_eq!(t.state(), (8, 8, 0));
        t.hit();
        assert_eq!(t.state(), (4, 8, 1));
        t.hit();
        t.hit();
        t.hit();
        assert_eq!(t.window(), 1, "never below one");
        for _ in 0..RECOVER_AFTER - 1 {
            t.clean();
        }
        assert_eq!(t.window(), 1);
        t.clean();
        assert_eq!(t.window(), 2);
        // A hit in the middle of a recovery starts the count again.
        for _ in 0..RECOVER_AFTER - 1 {
            t.clean();
        }
        t.hit();
        for _ in 0..RECOVER_AFTER - 1 {
            t.clean();
        }
        assert_eq!(t.window(), 1);
        for _ in 0..RECOVER_AFTER * 20 {
            t.clean();
        }
        assert_eq!(t.window(), 8, "never above the ceiling");
    }
}
