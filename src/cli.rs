use clap::Parser;
use std::ffi::OsString;

/// Resource-aware parallel builds for Linux.
#[derive(Debug, Parser)]
#[command(
    version,
    arg_required_else_help = true,
    after_help = "Supports Cargo, GNU Make >= 4.4, Ninja >= 1.13, and CMake builds.\nDo not pass explicit parallelism options (-j/--jobs/--parallel)."
)]
pub struct Cli {
    /// Build command and its arguments (cargo, make, ninja, or cmake --build).
    #[arg(required = true, trailing_var_arg = true, num_args = 1.., value_name = "COMMAND")]
    pub command: Vec<OsString>,
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
    fn requires_a_command() {
        assert!(Cli::try_parse_from(["make-pace"]).is_err());
        assert!(Cli::try_parse_from(["make-pace", "--"]).is_err());
    }
}
