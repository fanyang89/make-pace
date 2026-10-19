// No libtest harness: FIFO attachment temporarily changes the environment and
// must run in a genuinely single-threaded process.
#[path = "../src/jobserver.rs"]
mod jobserver;

use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::Command,
};

fn main() {
    for previous in [None, Some("sentinel flags")] {
        // SAFETY: this executable never starts threads.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("CARGO_MAKEFLAGS", value),
                None => std::env::remove_var("CARGO_MAKEFLAGS"),
            }
        }
        // SAFETY: this executable never starts threads.
        let mut server = unsafe { jobserver::Jobserver::new() }.unwrap();
        assert_eq!(std::env::var("CARGO_MAKEFLAGS").ok().as_deref(), previous);
        let mut command = Command::new("make");
        server.configure(&mut command);
        let flags = command
            .get_envs()
            .find(|(key, _)| *key == "MAKEFLAGS")
            .unwrap()
            .1
            .unwrap()
            .to_str()
            .unwrap();
        assert!(!flags.contains("--jobserver-fds="));
        let path = PathBuf::from(
            flags
                .split_whitespace()
                .find_map(|flag| flag.strip_prefix("--jobserver-auth=fifo:"))
                .unwrap(),
        );
        let dir = path.parent().unwrap().to_path_buf();
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for key in ["CARGO_MAKEFLAGS", "MFLAGS"] {
            assert_eq!(
                command
                    .get_envs()
                    .find(|(name, _)| *name == key)
                    .unwrap()
                    .1
                    .unwrap(),
                flags,
            );
        }
        server.resize(4).unwrap();
        assert_eq!(server.issued, 3);
        let mut client = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        let mut token = [0];
        client.read_exact(&mut token).unwrap();
        server.resize(1).unwrap();
        assert_eq!(server.issued, 1);
        // Empty FIFO with an in-flight token must not block contraction.
        server.resize(1).unwrap();
        assert_eq!(server.issued, 1);
        client.write_all(&token).unwrap();
        server.resize(1).unwrap();
        assert_eq!(server.issued, 0);
        server.resize(3).unwrap();
        assert_eq!(server.issued, 2);
        server.resize(0).unwrap();
        assert_eq!(server.issued, 0);
        drop(server);
        assert!(!dir.exists());
    }
    println!("PASS FIFO tokens, inheritance, environment restoration and cleanup");
}
