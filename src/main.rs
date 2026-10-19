#[cfg(not(target_os = "linux"))]
compile_error!("make-pace currently supports Linux only");
mod cli;
mod control;
mod jobserver;
mod resources;

use clap::Parser;
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use signal_hook::{
    consts::{SIGHUP, SIGINT, SIGTERM},
    iterator::Signals,
};
use std::{
    env,
    ffi::OsString,
    io,
    os::unix::process::{CommandExt, ExitStatusExt},
    path::Path,
    process::{Command, ExitStatus},
    time::{Duration, Instant},
};
fn version_at_least(program: &str, required: (u32, u32)) -> io::Result<()> {
    let out = Command::new(program).arg("--version").output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let version = text.split_whitespace().find_map(|word| {
        let mut parts = word.split('.');
        Some((
            parts.next()?.parse::<u32>().ok()?,
            parts.next()?.parse::<u32>().ok()?,
        ))
    });
    if !out.status.success() || !version.is_some_and(|v| v >= required) {
        return Err(io::Error::other(format!(
            "{program} >= {}.{} is required for FIFO jobserver support",
            required.0, required.1
        )));
    }
    Ok(())
}
fn validate(args: &[OsString]) -> io::Result<bool> {
    let mut needs_fifo = false;
    for arg in &args[1..] {
        let s = arg.to_string_lossy();
        if (s.starts_with('-') && !s.starts_with("--") && s.contains('j'))
            || s == "--jobs"
            || s.starts_with("--jobs=")
            || s == "--parallel"
            || s.starts_with("--parallel=")
        {
            return Err(io::Error::other(
                "explicit parallelism overrides the jobserver; remove -j/--jobs/--parallel",
            ));
        }
    }
    for key in ["CMAKE_BUILD_PARALLEL_LEVEL", "CARGO_BUILD_JOBS"] {
        if env::var_os(key).is_some() {
            return Err(io::Error::other(format!(
                "unset {key}; parallelism is managed by the jobserver"
            )));
        }
    }
    let name = Path::new(&args[0])
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    match name {
        "cargo" => (),
        "make" | "gmake" => version_at_least(args[0].to_str().unwrap(), (4, 4))?,
        "ninja" => {
            needs_fifo = true;
            version_at_least(args[0].to_str().unwrap(), (1, 13))?;
        }
        "cmake" => {
            let index = args.iter().position(|a| a == "--build").ok_or_else(|| {
                io::Error::other("use cmake --build <directory>; configure separately")
            })?;
            let dir = args
                .get(index + 1)
                .ok_or_else(|| io::Error::other("missing cmake build directory"))?;
            let cache = std::fs::read_to_string(Path::new(dir).join("CMakeCache.txt"))?;
            let generator = cache
                .lines()
                .find_map(|s| s.strip_prefix("CMAKE_GENERATOR:INTERNAL="))
                .unwrap_or("");
            let program = cache
                .lines()
                .find_map(|s| s.strip_prefix("CMAKE_MAKE_PROGRAM:FILEPATH="))
                .ok_or_else(|| io::Error::other("missing CMAKE_MAKE_PROGRAM"))?;
            match generator {
                "Ninja" | "Ninja Multi-Config" => {
                    needs_fifo = true;
                    version_at_least(program, (1, 13))?;
                }
                "Unix Makefiles" => version_at_least(program, (4, 4))?,
                _ => {
                    return Err(io::Error::other(format!(
                        "unsupported CMake generator: {generator}"
                    )));
                }
            }
        }
        _ => {
            return Err(io::Error::other(
                "supported commands: cargo, make, ninja, cmake --build",
            ));
        }
    }
    Ok(needs_fifo)
}
fn inherited() -> Option<OsString> {
    ["CARGO_MAKEFLAGS", "MAKEFLAGS"]
        .iter()
        .filter_map(env::var_os)
        .find(|s| {
            let s = s.to_string_lossy();
            s.contains("--jobserver-auth=") || s.contains("--jobserver-fds=")
        })
}
fn send_group(pid: u32, signal: Signal) {
    // The child was placed in a process group with this ID; it may have exited.
    let _ = killpg(Pid::from_raw(pid as i32), signal);
}
fn run(cli: cli::Cli) -> io::Result<Option<ExitStatus>> {
    let inherited = inherited();
    if inherited.is_some() && (cli.max_jobs.is_some() || cli.ramp_interval.is_some()) {
        return Err(io::Error::other(
            "--max-jobs and --ramp-interval cannot override an inherited jobserver",
        ));
    }
    let policy = cli.policy();
    if cli.explain {
        let snapshot = resources::sample()?;
        println!("Effective CPU capacity: {:.2}", snapshot.cpus);
        println!(
            "Memory limit: {:.2} GiB",
            snapshot.memory_limit as f64 / (1u64 << 30) as f64
        );
        println!(
            "Available memory: {:.2} GiB",
            snapshot.available as f64 / (1u64 << 30) as f64
        );
        if inherited.is_some() {
            println!(
                "Mode: inherited jobserver (adaptive control disabled; parent owns the limits)"
            );
        } else {
            println!("Mode: adaptive FIFO jobserver");
            println!("Initial job slots: 1 (implicit slot)");
            println!(
                "Job slot ceiling: {} (CPU-derived bound, capped at {})",
                policy.ceiling(snapshot.cpus),
                policy.max_jobs
            );
            println!(
                "Ramp interval: {} of healthy samples (+1 slot, sampled every second)",
                humantime::format_duration(policy.ramp_interval)
            );
            println!("Pressure reduces concurrency; running jobs are not interrupted.");
        }
        println!(
            "Only visible host/cgroup constraints are detected; this is not a hard resource cap."
        );
        return Ok(None);
    }
    let args = cli.command;
    let needs_fifo = validate(&args)?;
    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    let mut cmd = Command::new(&args[0]);
    cmd.args(&args[1..]).process_group(0);
    let mut server;
    let mut controller;
    if let Some(flags) = inherited {
        if needs_fifo && !flags.to_string_lossy().contains("--jobserver-auth=fifo:") {
            return Err(io::Error::other(
                "Ninja requires an inherited FIFO jobserver; anonymous-pipe jobservers are unsupported",
            ));
        }
        for key in ["MAKEFLAGS", "CARGO_MAKEFLAGS", "MFLAGS"] {
            cmd.env(key, &flags);
        }
        eprintln!(
            "make-pace: inherited jobserver; preserving parent policy (adaptive control disabled)"
        );
        server = None;
        controller = None;
    } else {
        // Preserve ordinary make options, but reject ambiguous pre-existing parallelism.
        let old = env::var("MAKEFLAGS").unwrap_or_default();
        if old
            .split_whitespace()
            .any(|s| s.starts_with("-j") || s.starts_with("--jobs"))
        {
            return Err(io::Error::other(
                "remove parallelism options from MAKEFLAGS",
            ));
        }
        let initial = resources::sample()?;
        eprintln!(
            "make-pace: CPU {:.2}, memory limit {:.2} GiB, available {:.2} GiB",
            initial.cpus,
            initial.memory_limit as f64 / (1u64 << 30) as f64,
            initial.available as f64 / (1u64 << 30) as f64
        );
        // SAFETY: this binary has not started any threads; initialization happens
        // before spawning the build. No dependency here starts background threads.
        let j = unsafe { jobserver::Jobserver::new()? };
        j.configure(&mut cmd);
        // configure supplies authoritative jobserver flags; append non-jobserver flags.
        if !old.is_empty() {
            let flags = cmd
                .get_envs()
                .find(|(k, _)| *k == "MAKEFLAGS")
                .and_then(|(_, v)| v)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            cmd.env("MAKEFLAGS", format!("{old} {flags}"));
        }
        server = Some(j);
        controller = Some(control::Controller::new(initial, policy));
    }
    let mut child = cmd.spawn()?;
    let pid = child.id();
    let mut last_sample = Instant::now();
    let mut shutdown = None;
    let mut failure = None;
    let mut target = 1;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // The build leader has exited; remaining descendants must not retain
                // a jobserver whose backing directory is about to be removed.
                send_group(pid, Signal::SIGKILL);
                if let Some(error) = failure {
                    return Err(error);
                }
                return Ok(Some(status));
            }
            Ok(None) => (),
            Err(error) => {
                send_group(pid, Signal::SIGKILL);
                let _ = child.wait();
                return Err(error);
            }
        }
        for signal in signals.pending() {
            send_group(pid, Signal::try_from(signal).expect("registered signal"));
            shutdown.get_or_insert(Instant::now());
        }
        if shutdown.is_some_and(|t: Instant| t.elapsed() >= Duration::from_secs(5)) {
            send_group(pid, Signal::SIGKILL);
        }
        if shutdown.is_none()
            && let (Some(j), Some(c)) = (&mut server, &mut controller)
        {
            let result = (|| {
                if last_sample.elapsed() >= Duration::from_secs(1) {
                    let now = resources::sample()?;
                    let elapsed = last_sample.elapsed();
                    last_sample = Instant::now();
                    let next = c.update(now, elapsed);
                    if next != target {
                        eprintln!("make-pace: jobs {target} -> {next}");
                        target = next;
                    }
                }
                // Retry pending contraction more often than resource sampling.
                j.resize(target)
            })();
            if let Err(error) = result {
                failure = Some(error);
                send_group(pid, Signal::SIGTERM);
                shutdown = Some(Instant::now());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn main() {
    let cli = cli::Cli::parse();
    match run(cli) {
        Ok(None) => (),
        Ok(Some(status)) => std::process::exit(
            status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)),
        ),
        Err(error) => {
            eprintln!("make-pace: {error}");
            std::process::exit(1);
        }
    }
}
