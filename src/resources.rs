use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
struct Group {
    path: PathBuf,
    v2: bool,
    cpu: bool,
    memory: bool,
}

#[derive(Debug)]
pub struct Snapshot {
    pub cpus: f64,
    pub available: u64,
    pub memory_limit: u64,
    pub cpu_total: u64,
    pub cpu_busy: u64,
    pub group_cpu_us: Option<u64>,
    pub throttled: u64,
    pub pressure: f64,
}

fn read(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok()
}
fn number(path: impl AsRef<Path>) -> Option<u64> {
    read(path)?.trim().parse().ok()
}
fn field(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next()? == key)
            .then(|| words.next()?.parse().ok())
            .flatten()
    })
}
fn unescape(s: &str) -> String {
    s.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

// Walk only ancestors visible within this controller's mount.
fn groups(memberships: &str, mounts: &str) -> Vec<Group> {
    let mut result = Vec::new();
    for line in mounts.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let a: Vec<_> = left.split_whitespace().collect();
        let b: Vec<_> = right.split_whitespace().collect();
        if a.len() < 5 || b.len() < 3 || !matches!(b[0], "cgroup" | "cgroup2") {
            continue;
        }
        let v2 = b[0] == "cgroup2";
        let options: Vec<_> = b[2].split(',').collect();
        let cpu = v2 || options.contains(&"cpu");
        let memory = v2 || options.contains(&"memory");
        if !cpu && !memory && !options.contains(&"cpuset") && !options.contains(&"cpuacct") {
            continue;
        }
        for entry in memberships.lines() {
            let parts: Vec<_> = entry.splitn(3, ':').collect();
            if parts.len() != 3 {
                continue;
            }
            let controllers: Vec<_> = parts[1].split(',').collect();
            if !(v2 && parts[1].is_empty()
                || !v2 && controllers.iter().any(|c| options.contains(c)))
            {
                continue;
            }
            let root = PathBuf::from(unescape(a[3]));
            let mount = PathBuf::from(unescape(a[4]));
            let member = PathBuf::from(parts[2]);
            let relative = member.strip_prefix(&root).ok().or_else(|| {
                // A cgroup namespace can expose membership relative to its root.
                (member == Path::new("/")).then_some(Path::new(""))
            });
            let Some(relative) = relative else { continue };
            if relative
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                continue;
            }
            let mut path = mount.join(relative);
            loop {
                result.push(Group {
                    path: path.clone(),
                    v2,
                    cpu,
                    memory,
                });
                if path == mount || !path.pop() {
                    break;
                }
            }
        }
    }
    result
}

fn cpuset_count(s: &str) -> Option<usize> {
    let mut count = 0usize;
    for part in s.trim().split(',') {
        let (lo, hi) = part.split_once('-').unwrap_or((part, part));
        let lo: usize = lo.parse().ok()?;
        let hi: usize = hi.parse().ok()?;
        count = count.checked_add(hi.checked_sub(lo)?.checked_add(1)?)?;
    }
    (count > 0).then_some(count)
}
fn quota(s: &str) -> Option<f64> {
    let mut words = s.split_whitespace();
    let quota: f64 = words.next()?.parse().ok()?;
    let period: f64 = words.next()?.parse().ok()?;
    (quota > 0.0 && period > 0.0).then_some(quota / period)
}
fn pressure(text: &str) -> f64 {
    text.lines()
        .filter(|s| s.starts_with("some "))
        .flat_map(str::split_whitespace)
        .find_map(|s| s.strip_prefix("avg10=")?.parse().ok())
        .unwrap_or(0.0)
}

pub fn sample() -> io::Result<Snapshot> {
    let mem = fs::read_to_string("/proc/meminfo")?;
    let total =
        field(&mem, "MemTotal:").ok_or_else(|| io::Error::other("missing MemTotal"))? * 1024;
    let available = field(&mem, "MemAvailable:")
        .ok_or_else(|| io::Error::other("missing MemAvailable"))?
        * 1024;
    let stat = fs::read_to_string("/proc/stat")?;
    let times: Vec<u64> = stat
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .skip(1)
        .take(8)
        .filter_map(|s| s.parse().ok())
        .collect();
    if times.len() < 5 {
        return Err(io::Error::other("invalid /proc/stat"));
    }
    let cpu_total = times.iter().sum::<u64>();
    let status = fs::read_to_string("/proc/self/status")?;
    let affinity = status
        .lines()
        .find_map(|s| s.strip_prefix("Cpus_allowed_list:").and_then(cpuset_count))
        .ok_or_else(|| io::Error::other("missing CPU affinity"))?;
    let online = read("/sys/devices/system/cpu/online")
        .and_then(|s| cpuset_count(&s))
        .unwrap_or(affinity);
    let mut s = Snapshot {
        cpus: affinity.min(online) as f64,
        available,
        memory_limit: total,
        cpu_total,
        cpu_busy: cpu_total - times[3] - times[4],
        group_cpu_us: None,
        throttled: 0,
        pressure: read("/proc/pressure/memory")
            .map(|s| pressure(&s))
            .unwrap_or(0.0),
    };
    let memberships = fs::read_to_string("/proc/self/cgroup")?;
    let mounts = fs::read_to_string("/proc/self/mountinfo")?;
    for g in groups(&memberships, &mounts) {
        apply_group(&mut s, &g);
    }
    Ok(s)
}

fn apply_group(s: &mut Snapshot, g: &Group) {
    let p = &g.path;
    let set = if g.v2 {
        "cpuset.cpus.effective"
    } else {
        "cpuset.cpus"
    };
    if let Some(n) = read(p.join(set)).and_then(|s| cpuset_count(&s)) {
        s.cpus = s.cpus.min(n as f64);
    }
    if g.cpu {
        let q = if g.v2 {
            read(p.join("cpu.max")).and_then(|s| quota(&s))
        } else {
            read(p.join("cpu.cfs_quota_us"))
                .zip(read(p.join("cpu.cfs_period_us")))
                .and_then(|(a, b)| quota(&format!("{} {}", a.trim(), b.trim())))
        };
        if let Some(q) = q {
            s.cpus = s.cpus.min(q);
        }
        if let Some(stat) = read(p.join("cpu.stat")) {
            s.throttled = s
                .throttled
                .saturating_add(field(&stat, "nr_throttled").unwrap_or(0));
            // First entry is the leaf; parent usage must not replace it.
            if g.v2 && s.group_cpu_us.is_none() {
                s.group_cpu_us = field(&stat, "usage_usec");
            }
        }
    }
    if !g.v2 && s.group_cpu_us.is_none() {
        s.group_cpu_us = number(p.join("cpuacct.usage")).map(|n| n / 1000);
    }
    if g.memory {
        let limits: &[(&str, &str)] = if g.v2 {
            &[
                ("memory.max", "memory.current"),
                ("memory.high", "memory.current"),
            ]
        } else {
            &[("memory.limit_in_bytes", "memory.usage_in_bytes")]
        };
        for (limit_file, used_file) in limits {
            if let Some(limit) = number(p.join(limit_file)).filter(|n| *n < (1u64 << 60)) {
                s.memory_limit = s.memory_limit.min(limit);
                // Fail closed if a finite limit is visible but its usage is not.
                let used = number(p.join(used_file)).unwrap_or(limit);
                s.available = s.available.min(limit.saturating_sub(used));
            }
        }
        if let Some(text) = read(p.join("memory.pressure")) {
            s.pressure = s.pressure.max(pressure(&text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_limits() {
        assert_eq!(cpuset_count("0-3,8,10-11\n"), Some(7));
        assert_eq!(cpuset_count("3-1"), None);
        assert_eq!(quota("150000 100000"), Some(1.5));
        assert_eq!(quota("max 100000"), None);
        assert_eq!(quota("-1 100000"), None);
    }
    #[test]
    fn v2_ancestors_and_mount_root() {
        let g = groups(
            "0::/tenant/build\n",
            "1 0 0:1 /tenant /sys/fs/cgroup rw - cgroup2 cgroup rw\n",
        );
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].path, Path::new("/sys/fs/cgroup/build"));
        assert_eq!(g[1].path, Path::new("/sys/fs/cgroup"));
    }
    #[test]
    fn v1_split_controllers() {
        let g = groups(
            "2:cpu,cpuacct:/a\n3:memory:/a\n",
            "1 0 0:1 / /cg/cpu rw - cgroup cgroup rw,cpu,cpuacct\n2 0 0:2 / /cg/mem rw - cgroup cgroup rw,memory\n",
        );
        assert_eq!(g.len(), 4);
        assert!(g[0].cpu && !g[0].memory);
        assert!(g[2].memory && !g[2].cpu);
    }
    #[test]
    fn namespace_root_and_escaped_mount() {
        let g = groups(
            "0::/\n",
            "1 0 0:1 /tenant /cg\\040root rw - cgroup2 cgroup rw\n",
        );
        assert_eq!(g[0].path, Path::new("/cg root"));
    }
    #[test]
    fn live_sample() {
        let s = sample().unwrap();
        assert!(s.cpus > 0.0);
        assert!(s.memory_limit > 0);
    }
    #[test]
    fn reads_v1_v2_limits_and_shared_parent_usage() {
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join("tmp"));
        let dir = base
            .join("pi")
            .join(format!("resource-fixture-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let leaf = dir.join("leaf");
        fs::create_dir_all(&leaf).unwrap();
        let mut s = Snapshot {
            cpus: 16.0,
            available: 9000,
            memory_limit: 10000,
            cpu_total: 0,
            cpu_busy: 0,
            group_cpu_us: None,
            throttled: 0,
            pressure: 0.0,
        };
        for (name, value) in [
            ("memory.max", "8000"),
            ("memory.current", "1000"),
            ("cpu.max", "150000 100000"),
            ("cpuset.cpus.effective", "0-3"),
        ] {
            fs::write(leaf.join(name), value).unwrap();
        }
        apply_group(
            &mut s,
            &Group {
                path: leaf,
                v2: true,
                cpu: true,
                memory: true,
            },
        );
        assert_eq!(s.cpus, 1.5);
        assert_eq!(s.available, 7000);
        for (name, value) in [
            ("memory.max", "9000"),
            ("memory.current", "8500"),
            ("cpu.max", "max 100000"),
        ] {
            fs::write(dir.join(name), value).unwrap();
        }
        apply_group(
            &mut s,
            &Group {
                path: dir.clone(),
                v2: true,
                cpu: true,
                memory: true,
            },
        );
        assert_eq!(s.memory_limit, 8000);
        assert_eq!(s.available, 500);
        assert_eq!(s.cpus, 1.5);
        for (name, value) in [
            ("memory.limit_in_bytes", "4096"),
            ("memory.usage_in_bytes", "4000"),
            ("cpu.cfs_quota_us", "50000"),
            ("cpu.cfs_period_us", "100000"),
        ] {
            fs::write(dir.join(name), value).unwrap();
        }
        let group = Group {
            path: dir.clone(),
            v2: false,
            cpu: true,
            memory: true,
        };
        apply_group(&mut s, &group);
        assert_eq!(s.cpus, 0.5);
        assert_eq!(s.memory_limit, 4096);
        assert_eq!(s.available, 96);
        fs::remove_file(dir.join("memory.usage_in_bytes")).unwrap();
        apply_group(&mut s, &group);
        assert_eq!(s.available, 0);
        fs::remove_dir_all(dir).unwrap();
    }
}
