use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct RenderScheduler {
    requested: bool,
    force: bool,
    min_interval: Duration,
    last_render_at: Option<Instant>,
}

impl RenderScheduler {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            requested: false,
            force: false,
            min_interval,
            last_render_at: None,
        }
    }

    pub fn request(&mut self, force: bool) {
        self.requested = true;
        self.force |= force;
    }

    pub fn has_pending(&self) -> bool {
        self.requested
    }

    pub fn next_render_at(&self, now: Instant) -> Option<Instant> {
        if !self.requested {
            return None;
        }
        if self.force {
            return Some(now);
        }
        let Some(last) = self.last_render_at else {
            return Some(now);
        };
        let next = last + self.min_interval;
        Some(if now >= next { now } else { next })
    }

    pub fn should_render_now(&self, now: Instant) -> bool {
        self.next_render_at(now)
            .map(|deadline| deadline <= now)
            .unwrap_or(false)
    }

    pub fn mark_rendered(&mut self, now: Instant) -> bool {
        if !self.requested {
            return false;
        }
        self.requested = false;
        self.force = false;
        self.last_render_at = Some(now);
        true
    }
}
