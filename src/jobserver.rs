use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

pub struct Jobserver {
    dir: PathBuf,
    fifo: File,
    pub issued: usize,
}
impl Jobserver {
    pub fn new() -> io::Result<Self> {
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
            let fifo = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(dir.join("tokens"))?;
            Ok(Self {
                dir: dir.clone(),
                fifo,
                issued: 0,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&dir);
        }
        result
    }
    pub fn configure(&self, cmd: &mut Command) {
        let flags = format!(
            "-j --jobserver-auth=fifo:{}",
            self.dir.join("tokens").display()
        );
        for key in ["MAKEFLAGS", "CARGO_MAKEFLAGS", "MFLAGS"] {
            cmd.env(key, &flags);
        }
    }
    pub fn resize(&mut self, target: usize) -> io::Result<()> {
        // The top-level build owns one implicit slot. No in-flight token is revoked.
        let desired = target.saturating_sub(1);
        while self.issued < desired {
            match self.fifo.write(b"+") {
                Ok(1) => self.issued += 1,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
                _ => return Err(io::Error::other("short FIFO write")),
            }
        }
        while self.issued > desired {
            match self.fifo.read(&mut [0u8; 1]) {
                Ok(1) => self.issued -= 1,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
                _ => return Err(io::Error::other("unexpected FIFO EOF")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn tokens_and_cleanup() {
        let mut j = Jobserver::new().unwrap();
        let path = j.dir.clone();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        j.resize(4).unwrap();
        assert_eq!(j.issued, 3);
        let mut client = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path.join("tokens"))
            .unwrap();
        let mut token = [0];
        client.read_exact(&mut token).unwrap();
        j.resize(1).unwrap();
        assert_eq!(j.issued, 1);
        client.write_all(&token).unwrap();
        j.resize(1).unwrap();
        assert_eq!(j.issued, 0);
        drop(j);
        assert!(!path.exists());
    }
}
