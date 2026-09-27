//! Shared lock for filesystem publication and scans. Python's catalog_sync.py
//! uses the same /dados/.catalog-sync.lock via fcntl.flock(LOCK_EX).
use std::{fs::{File, OpenOptions}, io, os::fd::AsRawFd, path::Path};

pub struct CatalogLock(File);

impl CatalogLock {
    pub fn acquire(data_dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let file = OpenOptions::new().create(true).read(true).write(true)
            .open(data_dir.join(".catalog-sync.lock"))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(file))
    }
}

impl Drop for CatalogLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}
