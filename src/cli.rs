use clap::Parser;
use std::{ffi::OsString, time::Duration};

/// Resource-aware parallel builds for Linux.
#[derive(Debug, Parser)]
#[command(
    version,
    arg_required_else_help = true,
    after_help = "Supports Cargo, GNU Make >= 4.4, Ninja >= 1.13, and CMake builds.\nDo not pass explicit parallelism options (-j/--jobs/--parallel)."
)]
pub struct Cli {
    /// Upper bound on job slots, including the implicit slot (1-4096).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=4096))]
    pub max_jobs: Option<u32>,

    /// Healthy time before adding one slot (default: 3s; sampled every second).
    #[arg(long, value_parser = parse_interval, value_name = "DURATION")]
    pub ramp_interval: Option<Duration>,

    /// Show detected resources and policy without starting a build.
    #[arg(long, conflicts_with = "command")]
    pub explain: bool,

    /// Build command and its arguments (cargo, make, ninja, or cmake --build).
    #[arg(required_unless_present = "explain", trailing_var_arg = true, num_args = 1.., value_name = "COMMAND")]
    pub command: Vec<OsString>,
}

fn parse_interval(value: &str) -> Result<Duration, String> {
    let duration = humantime::parse_duration(value).map_err(|error| error.to_string())?;
    if duration.is_zero() {
        return Err("interval must be greater than zero".into());
    }
    Ok(duration)
}

impl Cli {
    pub fn policy(&self) -> crate::control::Policy {
        let defaults = crate::control::Policy::default();
        crate::control::Policy {
            max_jobs: self.max_jobs.map_or(defaults.max_jobs, |n| n as usize),
            ramp_interval: self.ramp_interval.unwrap_or(defaults.ramp_interval),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, error::ErrorKind};
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn valid_definition() {
        Cli::command().debug_assert();
    }

    #[test]
    fn forwards_build_arguments() {
        for separator in [false, true] {
            for command in [
                vec!["cargo", "build", "--release"],
                vec!["cmake", "--build", "build dir"],
                vec!["make", "-j4"],
                vec!["ninja", "--help"],
                vec!["cargo", "run", "--", "--version"],
            ] {
                let mut args = vec!["make-pace"];
                if separator {
                    args.push("--");
                }
                args.extend(&command);
                let cli = Cli::try_parse_from(args).unwrap();
                assert_eq!(
                    cli.command,
                    command.iter().map(OsString::from).collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn preserves_non_utf8_arguments() {
        let path = OsString::from_vec(b"build-\xff".to_vec());
        let cli = Cli::try_parse_from([
            OsString::from("make-pace"),
            OsString::from("--"),
            OsString::from("ninja"),
            OsString::from("-C"),
            path.clone(),
        ])
        .unwrap();
        assert_eq!(cli.command[2], path);
    }

    #[test]
    fn standard_help_and_version() {
        for (arg, kind) in [
            ("--help", ErrorKind::DisplayHelp),
            ("-h", ErrorKind::DisplayHelp),
            ("--version", ErrorKind::DisplayVersion),
            ("-V", ErrorKind::DisplayVersion),
            ("--unknown", ErrorKind::UnknownArgument),
        ] {
            assert_eq!(
                Cli::try_parse_from(["make-pace", arg]).unwrap_err().kind(),
                kind
            );
        }
    }

    #[test]
    fn policy_options_and_explain() {
        let cli = Cli::try_parse_from([
            "make-pace",
            "--max-jobs",
            "6",
            "--ramp-interval",
            "2s",
            "--explain",
        ])
        .unwrap();
        assert!(cli.explain);
        assert!(cli.command.is_empty());
        assert_eq!(cli.policy().max_jobs, 6);
        assert_eq!(cli.policy().ramp_interval, Duration::from_secs(2));
        let cli = Cli::try_parse_from(["make-pace", "--explain"]).unwrap();
        assert_eq!(cli.policy().ramp_interval, Duration::from_secs(3));
        assert!(Cli::try_parse_from(["make-pace", "--explain", "--", "make"]).is_err());
        assert!(Cli::try_parse_from(["make-pace", "--max-jobs", "4"]).is_err());
    }

    #[test]
    fn rejects_invalid_policy_options() {
        for value in ["0", "4097", "-1", "many"] {
            assert!(Cli::try_parse_from(["make-pace", "--max-jobs", value, "--explain"]).is_err());
        }
        for value in ["0s", "-1s", "never"] {
            assert!(
                Cli::try_parse_from(["make-pace", "--ramp-interval", value, "--explain"]).is_err()
            );
        }
    }

    #[test]
    fn wrapper_options_do_not_capture_child_options() {
        let cli = Cli::try_parse_from([
            "make-pace",
            "--max-jobs",
            "2",
            "--",
            "cargo",
            "run",
            "--",
            "--max-jobs",
            "8",
        ])
        .unwrap();
        assert_eq!(cli.max_jobs, Some(2));
        assert_eq!(
            cli.command,
            ["cargo", "run", "--", "--max-jobs", "8"].map(OsString::from)
        );
    }

    #[test]
    fn requires_a_command() {
        assert!(Cli::try_parse_from(["make-pace"]).is_err());
        assert!(Cli::try_parse_from(["make-pace", "--"]).is_err());
    }
}
