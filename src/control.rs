use crate::resources::Snapshot;
use std::time::Duration;

pub struct Controller {
    pub target: usize,
    previous: Snapshot,
    growth: f64,
    healthy: u32,
    cooldown: u32,
}
impl Controller {
    pub fn new(initial: Snapshot) -> Self {
        Self {
            target: 1,
            previous: initial,
            growth: 0.0,
            healthy: 0,
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
        let ceiling = ((now.cpus * 2.0).ceil() as usize).clamp(1, 4096);
        self.target = self.target.min(ceiling);
        if danger || throttling {
            if self.cooldown == 0 || now.available <= reserve {
                self.target = (self.target * 3 / 4).max(1);
                self.cooldown = 5;
            }
            self.healthy = 0;
        } else if self.cooldown == 0 && host_busy < 0.9 && group_busy < 0.9 && headroom > reserve {
            self.healthy += 1;
            if self.healthy >= 3 {
                self.target = (self.target + 1).min(ceiling);
                self.healthy = 0;
            }
        } else {
            self.healthy = 0;
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
    fn slow_growth_fast_reduction() {
        let mut c = Controller::new(sample(800_000_000, 0));
        for t in 1..=9 {
            c.update(sample(800_000_000, t), Duration::from_secs(1));
        }
        assert_eq!(c.target, 4);
        assert_eq!(c.update(sample(50_000_000, 10), Duration::from_secs(1)), 3);
        assert_eq!(c.update(sample(800_000_000, 11), Duration::from_secs(1)), 3);
    }
    #[test]
    fn bounded_exploration() {
        let mut c = Controller::new(sample(800_000_000, 0));
        for t in 1..=100 {
            c.update(sample(800_000_000, t), Duration::from_secs(1));
        }
        assert_eq!(c.target, 8);
    }
    #[test]
    fn saturated_cpu_does_not_expand() {
        let mut c = Controller::new(sample(800_000_000, 0));
        for t in 1..=10 {
            let mut s = sample(800_000_000, t);
            s.cpu_busy = s.cpu_total;
            c.update(s, Duration::from_secs(1));
        }
        assert_eq!(c.target, 1);
    }
}
