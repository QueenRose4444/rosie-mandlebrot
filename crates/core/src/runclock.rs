//! Wall-clock GPU time budget. The duration counts time *spent rendering*, not
//! real-world elapsed time, so a 2h budget across two reboots still yields 2h
//! of GPU work.

use std::time::{Duration, Instant};

pub struct RunClock {
    started_this_process: Instant,
    elapsed_before_process: Duration,
    budget: Option<Duration>,
}

impl RunClock {
    pub fn new(elapsed_before: Duration, budget: Option<Duration>) -> Self {
        Self {
            started_this_process: Instant::now(),
            elapsed_before_process: elapsed_before,
            budget,
        }
    }

    pub fn total_elapsed(&self) -> Duration {
        self.elapsed_before_process + self.started_this_process.elapsed()
    }

    pub fn budget(&self) -> Option<Duration> {
        self.budget
    }

    pub fn remaining(&self) -> Option<Duration> {
        self.budget
            .map(|b| b.saturating_sub(self.total_elapsed()))
    }

    pub fn expired(&self) -> bool {
        match self.budget {
            Some(b) => self.total_elapsed() >= b,
            None => false,
        }
    }
}
