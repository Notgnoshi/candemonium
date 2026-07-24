use std::time::{Duration, Instant};

/// Coalesces a burst of events into a single action.
///
/// Each trigger pushes the deadline back, so a burst of events results in one action after the
/// burst settles rather than one action per event. A steady stream of triggers spaced closer than
/// the window postpones the action indefinitely; callers that need an upper bound must enforce it
/// themselves.
pub struct Debounce {
    window: Duration,
    deadline: Option<Instant>,
}

impl Debounce {
    /// Create an idle Debounce that fires `window` after the most recent trigger.
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            deadline: None,
        }
    }

    /// Record an event at `now`, (re)scheduling the deadline for `now + window`.
    pub fn trigger(&mut self, now: Instant) {
        self.deadline = Some(now + self.window);
    }

    /// Check if the deadline has passed as of `now`.
    pub fn expired(&mut self, now: Instant) -> bool {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_after_window() {
        let mut d = Debounce::new(Duration::from_millis(500));
        let t0 = Instant::now();
        assert!(!d.expired(t0), "must not fire without a trigger");
        d.trigger(t0);
        assert!(!d.expired(t0 + Duration::from_millis(499)));
        assert!(d.expired(t0 + Duration::from_millis(500)));
        assert!(
            !d.expired(t0 + Duration::from_millis(501)),
            "must reset to idle after firing"
        );
    }

    #[test]
    fn retrigger_pushes_deadline_back() {
        let mut d = Debounce::new(Duration::from_millis(500));
        let t0 = Instant::now();
        d.trigger(t0);
        d.trigger(t0 + Duration::from_millis(400));
        assert!(!d.expired(t0 + Duration::from_millis(500)));
        assert!(d.expired(t0 + Duration::from_millis(900)));
    }
}
