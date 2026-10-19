use std::{
    ffi::CString,
    fs, io,
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

pub struct Jobserver {
    dir: PathBuf,
    client: ::jobserver::Client,
    pub issued: usize,
}
impl Jobserver {
    /// Create a private FIFO and connect the jobserver client.
    ///
    /// # Safety
    /// Must be called before any other threads are started: the crate only exposes
    /// environment-based FIFO attachment, so CARGO_MAKEFLAGS is temporarily changed.
    pub unsafe fn new() -> io::Result<Self> {
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir)
                    .join("tmp")
            })
            .join("pi");
        fs::create_dir_all(&base)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let dir = base.join(format!("resource-jobserver-{}-{nonce}", std::process::id()));
        // MAKEFLAGS cannot safely represent arbitrary whitespace in FIFO auth paths.
        if dir
            .as_os_str()
            .as_bytes()
            .iter()
            .any(u8::is_ascii_whitespace)
        {
            return Err(io::Error::other("TMPDIR must not contain whitespace"));
        }
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let result = (|| {
            let path = CString::new(dir.join("tokens").as_os_str().as_bytes())?;
            // SAFETY: path is a valid NUL-terminated string, mode is a valid permission mask.
            if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let fifo_path = dir.join("tokens");
            let fifo_path = fifo_path
                .to_str()
                .ok_or_else(|| io::Error::other("jobserver FIFO path must be valid UTF-8"))?;
            let previous = std::env::var_os("CARGO_MAKEFLAGS");
            // SAFETY: the caller guarantees single-threaded startup. The auth value
            // names our FIFO, so from_env_ext never takes ownership of inherited FDs.
            let result = unsafe {
                std::env::set_var(
                    "CARGO_MAKEFLAGS",
                    format!("--jobserver-auth=fifo:{fifo_path}"),
                );
                let result = ::jobserver::Client::from_env_ext(true).client;
                match previous {
                    Some(value) => std::env::set_var("CARGO_MAKEFLAGS", value),
                    None => std::env::remove_var("CARGO_MAKEFLAGS"),
                }
                result
            };
            let client = result.map_err(io::Error::other)?;
            Ok(Self {
                dir: dir.clone(),
                client,
                issued: 0,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&dir);
        }
        result
    }
    pub fn configure(&self, cmd: &mut Command) {
        self.client.configure_make(cmd);
        // jobserver emits legacy --jobserver-fds even for FIFO auth. Ninja
        // interprets that as an FD pair and ignores the entire jobserver.
        let flags = cmd
            .get_envs()
            .find(|(key, _)| *key == "MAKEFLAGS")
            .and_then(|(_, value)| value)
            .expect("configure_make sets MAKEFLAGS")
            .to_str()
            .expect("FIFO path was validated as UTF-8")
            .split_whitespace()
            .filter(|flag| !flag.starts_with("--jobserver-fds="))
            .collect::<Vec<_>>()
            .join(" ");
        for key in ["MAKEFLAGS", "CARGO_MAKEFLAGS", "MFLAGS"] {
            cmd.env(key, &flags);
        }
    }
    pub fn resize(&mut self, target: usize) -> io::Result<()> {
        // The top-level build owns one implicit slot. No in-flight token is revoked.
        let desired = target.saturating_sub(1);
        while self.issued < desired {
            match self.client.release_raw() {
                Ok(()) => self.issued += 1,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        while self.issued > desired {
            match self.client.try_acquire()? {
                Some(token) => {
                    token.drop_without_releasing();
                    self.issued -= 1;
                }
                None => break,
            }
        }
        Ok(())
    }
}
impl Drop for Jobserver {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
