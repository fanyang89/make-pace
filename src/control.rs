use crate::resources::Snapshot;
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub max_jobs: usize,
    pub ramp_interval: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_jobs: 4096,
            ramp_interval: Duration::from_secs(3),
        }
    }
}

impl Policy {
    pub fn ceiling(&self, cpus: f64) -> usize {
        // Keep the automatic CPU-based exploration bound even with an explicit cap.
        ((cpus * 2.0).ceil() as usize)
            .clamp(1, 4096)
            .min(self.max_jobs)
    }
}

pub struct Controller {
    pub target: usize,
    previous: Snapshot,
    growth: f64,
    healthy: Duration,
    policy: Policy,
    cooldown: u32,
}
impl Controller {
    pub fn new(initial: Snapshot, policy: Policy) -> Self {
        Self {
            target: 1,
            previous: initial,
            growth: 0.0,
            healthy: Duration::ZERO,
            policy,
            cooldown: 0,
        }
    }
    pub fn update(&mut self, now: Snapshot, elapsed: Duration) -> usize {
        let seconds = elapsed.as_secs_f64().max(0.001);
        let reserve = (now.memory_limit / 10).clamp(64 * 1024 * 1024, 2 * 1024 * 1024 * 1024);
        let reserve = reserve.min(now.memory_limit / 4);
        let growth = self.previous.available.saturating_sub(now.available) as f64 / seconds;
        self.growth = 0.7 * self.growth + 0.3 * growth;
        let headroom = now.available.saturating_sub(reserve);
        let danger = now.available <= reserve
            || now.pressure >= 1.0
            || (self.growth > 0.0 && (headroom as f64 / self.growth) < 5.0);
        let total = now.cpu_total.saturating_sub(self.previous.cpu_total);
        let busy = now.cpu_busy.saturating_sub(self.previous.cpu_busy);
        let host_busy = if total > 0 {
            busy as f64 / total as f64
        } else {
            1.0
        };
        let group_busy = now
            .group_cpu_us
            .zip(self.previous.group_cpu_us)
            .map(|(n, p)| n.saturating_sub(p) as f64 / (seconds * 1e6 * now.cpus))
            .unwrap_or(0.0);
        let throttling = now.throttled > self.previous.throttled;
        // Bound exploration even for I/O-heavy workloads. This is not a resource budget.
        let ceiling = self.policy.ceiling(now.cpus);
        self.target = self.target.min(ceiling);
        if danger || throttling {
            if self.cooldown == 0 || now.available <= reserve {
                self.target = (self.target * 3 / 4).max(1);
                self.cooldown = 5;
            }
            self.healthy = Duration::ZERO;
        } else if self.cooldown == 0
            && self.target < ceiling
            && host_busy < 0.9
            && group_busy < 0.9
            && headroom > reserve
        {
            self.healthy = self.healthy.saturating_add(elapsed);
            if self.healthy >= self.policy.ramp_interval {
                self.target = (self.target + 1).min(ceiling);
                self.healthy = Duration::ZERO;
            }
        } else {
            self.healthy = Duration::ZERO;
        }
        self.cooldown = self.cooldown.saturating_sub(1);
        self.previous = now;
        self.target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(available: u64, ticks: u64) -> Snapshot {
        Snapshot {
            cpus: 4.0,
            available,
            memory_limit: 1_000_000_000,
            cpu_total: ticks * 100,
            cpu_busy: ticks * 10,
            group_cpu_us: None,
            throttled: 0,
            pressure: 0.0,
        }
    }
    #[test]
    fn explicit_caps_include_the_implicit_slot() {
        for max_jobs in [1, 2, 4096] {
            let mut c = Controller::new(
                sample(800_000_000, 0),
                Policy {
                    max_jobs,
                    ..Policy::default()
                },
            );
            for t in 1..=100 {
                c.update(sample(800_000_000, t), Duration::from_secs(1));
                assert!(c.target <= max_jobs);
            }
            assert_eq!(c.target, max_jobs.min(8));
        }
    }

    #[test]
    fn ramp_uses_elapsed_time_not_sample_count() {
        let mut c = Controller::new(
            sample(800_000_000, 0),
            Policy {
                ramp_interval: Duration::from_millis(2500),
                ..Policy::default()
            },
        );
        for t in 1..=4 {
            assert_eq!(
                c.update(sample(800_000_000, t), Duration::from_millis(500)),
                1
            );
        }
        assert_eq!(
            c.update(sample(800_000_000, 5), Duration::from_millis(500)),
            2
        );
        // A delayed sample grants one slot, never a burst of accumulated slots.
        assert_eq!(c.update(sample(800_000_000, 6), Duration::from_secs(30)), 3);
        assert_eq!(
            c.update(sample(800_000_000, 7), Duration::from_millis(500)),
            3
        );
    }

    #[test]
    fn unhealthy_samples_reset_ramp_progress() {
        let mut c = Controller::new(sample(800_000_000, 0), Policy::default());
        c.update(sample(800_000_000, 1), Duration::from_secs(2));
        let mut busy = sample(800_000_000, 2);
        busy.cpu_busy = 110; // All of this interval's 100 ticks were busy.
        assert_eq!(c.update(busy, Duration::from_secs(1)), 1);
        assert_eq!(c.update(sample(800_000_000, 3), Duration::from_secs(2)), 1);
        assert_eq!(c.update(sample(800_000_000, 4), Duration::from_secs(1)), 2);
    }

    #[test]
    fn pressure_reduces_without_waiting_for_ramp() {
        let mut c = Controller::new(
            sample(800_000_000, 0),
            Policy {
                ramp_interval: Duration::from_secs(60),
                ..Policy::default()
            },
        );
        for t in 1..=3 {
            c.update(sample(800_000_000, t), Duration::from_secs(60));
        }
        assert_eq!(c.target, 4);
        assert_eq!(c.update(sample(50_000_000, 4), Duration::from_secs(1)), 3);
        assert_eq!(c.update(sample(50_000_000, 5), Duration::from_secs(1)), 2);
        // Recovery still observes cooldown and requires fresh healthy time.
        assert_eq!(c.update(sample(800_000_000, 6), Duration::from_secs(1)), 2);
    }

    #[test]
    fn changing_cpu_bound_does_not_bank_expansion_credit() {
        let mut c = Controller::new(sample(800_000_000, 0), Policy::default());
        let mut restricted = sample(800_000_000, 1);
        restricted.cpus = 0.5;
        assert_eq!(c.update(restricted, Duration::from_secs(60)), 1);
        assert_eq!(c.update(sample(800_000_000, 2), Duration::from_secs(1)), 1);
        assert_eq!(c.update(sample(800_000_000, 3), Duration::from_secs(2)), 2);
        let mut restricted = sample(800_000_000, 4);
        restricted.cpus = 0.5;
        assert_eq!(c.update(restricted, Duration::from_secs(1)), 1);
    }

    #[test]
    fn slow_growth_fast_reduction() {
        let mut c = Controller::new(sample(800_000_000, 0), Policy::default());
        for t in 1..=9 {
            c.update(sample(800_000_000, t), Duration::from_secs(1));
        }
        assert_eq!(c.target, 4);
        assert_eq!(c.update(sample(50_000_000, 10), Duration::from_secs(1)), 3);
        assert_eq!(c.update(sample(800_000_000, 11), Duration::from_secs(1)), 3);
    }
    #[test]
    fn bounded_exploration() {
        let mut c = Controller::new(sample(800_000_000, 0), Policy::default());
        for t in 1..=100 {
            c.update(sample(800_000_000, t), Duration::from_secs(1));
        }
        assert_eq!(c.target, 8);
    }
    #[test]
    fn saturated_cpu_does_not_expand() {
        let mut c = Controller::new(sample(800_000_000, 0), Policy::default());
        for t in 1..=10 {
            let mut s = sample(800_000_000, t);
            s.cpu_busy = s.cpu_total;
            c.update(s, Duration::from_secs(1));
        }
        assert_eq!(c.target, 1);
    }
}
