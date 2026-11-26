//! Per-company admission for calendar notifications; independent of other jobs.
use std::time::{Duration, Instant};

pub fn interval(raw: Option<&str>) -> Result<Duration, &'static str> {
    let raw = raw.unwrap_or("60");
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("notification sweep seconds must be an integer between 60 and 3600");
    }
    let seconds = raw
        .parse::<u64>()
        .map_err(|_| "notification sweep seconds out of range")?;
    if !(60..=3600).contains(&seconds) {
        return Err("notification sweep seconds must be between 60 and 3600");
    }
    Ok(Duration::from_secs(seconds))
}

pub struct NotificationSchedule {
    interval: Duration,
    next_due: Option<Instant>,
    failures: u32,
    held: bool,
}
impl NotificationSchedule {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            next_due: None,
            failures: 0,
            held: false,
        }
    }
    pub fn ready(&self, now: Instant) -> bool {
        !self.held && self.next_due.is_none_or(|due| now >= due)
    }
    pub fn succeeded(&mut self, now: Instant) {
        self.failures = 0;
        self.next_due = Some(now + self.interval);
    }
    pub fn before_delivery(&mut self, observed: Instant, remaining: Duration, completed: Instant) {
        self.succeeded(completed);
        self.next_due = Some((observed + remaining).min(completed + self.interval));
    }
    pub fn failed(&mut self, now: Instant, permanent_database_fault: bool) {
        self.held = permanent_database_fault;
        let cap = self.interval.max(Duration::from_secs(900));
        let delay = self.interval.saturating_mul(1 << self.failures).min(cap);
        self.failures = (self.failures + 1).min(4);
        self.next_due = Some(now + delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_delivery_deadline_is_not_skipped_by_hourly_cadence_or_slow_work() {
        let observed = Instant::now(); // 23:00 local, delivery at 23:30.
        let mut state = NotificationSchedule::new(Duration::from_secs(3600));
        state.before_delivery(observed, Duration::from_secs(1800), observed);
        assert!(!state.ready(observed + Duration::from_secs(1799)));
        assert!(state.ready(observed + Duration::from_secs(1800)));
        // Completion after the observed clock must not move the deadline.
        state.before_delivery(
            observed,
            Duration::from_secs(60),
            observed + Duration::from_secs(59),
        );
        assert!(state.ready(observed + Duration::from_secs(60)));
        state.before_delivery(
            observed,
            Duration::from_secs(60),
            observed + Duration::from_secs(65),
        );
        assert!(state.ready(observed + Duration::from_secs(65)));
    }

    #[test]
    fn success_waits_one_minute_instead_of_every_maintenance_tick() {
        let now = Instant::now();
        let mut state = NotificationSchedule::new(Duration::from_secs(60));
        assert!(state.ready(now));
        state.succeeded(now);
        assert!(!state.ready(now + Duration::from_secs(2)));
        assert!(!state.ready(now + Duration::from_secs(59)));
        assert!(state.ready(now + Duration::from_secs(60)));
    }

    #[test]
    fn transient_failures_back_off_and_success_resets_the_delay() {
        let mut now = Instant::now();
        let mut state = NotificationSchedule::new(Duration::from_secs(60));
        for seconds in [60, 120, 240, 480, 900, 900] {
            state.failed(now, false);
            assert!(!state.ready(now + Duration::from_secs(seconds - 1)));
            now += Duration::from_secs(seconds);
            assert!(state.ready(now));
        }
        state.succeeded(now);
        now += Duration::from_secs(60);
        assert!(state.ready(now));
        state.failed(now, false);
        assert!(state.ready(now + Duration::from_secs(60)));
    }

    #[test]
    fn permanent_fault_holds_only_that_company_until_restart() {
        let now = Instant::now();
        let mut held = NotificationSchedule::new(Duration::from_secs(60));
        let other = NotificationSchedule::new(Duration::from_secs(60));
        held.failed(now, true);
        assert!(!held.ready(now + Duration::from_secs(86400)));
        assert!(other.ready(now));
        assert!(NotificationSchedule::new(Duration::from_secs(60)).ready(now));
    }

    #[test]
    fn configuration_is_bounded_and_never_retries_faster_than_a_slower_cadence() {
        assert_eq!(interval(None), Ok(Duration::from_secs(60)));
        for value in ["", "2", "59", "3601", "-60", " 60", "abc"] {
            assert!(interval(Some(value)).is_err());
        }
        let now = Instant::now();
        let mut state = NotificationSchedule::new(interval(Some("1800")).expect("valid interval"));
        state.failed(now, false);
        assert!(!state.ready(now + Duration::from_secs(1799)));
        assert!(state.ready(now + Duration::from_secs(1800)));
    }
}
