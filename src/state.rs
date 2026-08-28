//! Shared state paths, clock, and private atomic persistence.
//!
//! State consumers share one XDG layout and epoch clock. Private writers also
//! share one cross-process transaction: a persistent sibling lock, a private
//! unique tempfile, and an atomic rename. Callers own their data format and
//! recovery policy; this module owns filesystem coordination.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The plugin's state directory: `$XDG_STATE_HOME/herdr-switchboard`, falling back to
/// `~/.local/state/herdr-switchboard`. `None` when neither var is set — callers then
/// skip the file entirely, which is the "no history / no cache" degrade.
pub fn state_dir() -> Option<PathBuf> {
    let base = env::var("XDG_STATE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/state"))
        })?;
    Some(base.join("herdr-switchboard"))
}

/// A file inside [`state_dir`], or `None` when there is no state dir.
pub fn state_file(name: &str) -> Option<PathBuf> {
    Some(state_dir()?.join(name))
}

/// Replace one state file atomically with bytes that are private from creation.
///
/// A persistent sibling lock serializes writers across Switchboard processes;
/// a unique sibling tempfile keeps each replacement isolated until rename.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let _lock = acquire_private_lock(path)?;
    replace_private(path, bytes)
}

/// Serialize a private read-modify-write transaction across processes.
///
/// The updater receives the current bytes when readable and returns both the
/// replacement bytes and the domain value the caller wants back. Missing or
/// unreadable input is `None`; an updater can therefore apply its own recovery
/// policy without learning anything about lock or tempfile mechanics.
pub fn update_private<T>(
    path: &Path,
    update: impl FnOnce(Option<&[u8]>) -> Result<(Vec<u8>, T)>,
) -> Result<T> {
    let _lock = acquire_private_lock(path)?;
    let current = fs::read(path).ok();
    let (replacement, value) = update(current.as_deref())?;
    replace_private(path, &replacement)?;
    Ok(value)
}

fn acquire_private_lock(path: &Path) -> Result<File> {
    create_parent(path)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("state file has no file name"))?
        .to_string_lossy();
    let lock_path = path.with_file_name(format!(".{file_name}.lock"));
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&lock_path)
        .with_context(|| format!("open state lock {}", lock_path.display()))?;
    file.lock()
        .with_context(|| format!("lock state file {}", path.display()))?;
    Ok(file)
}

fn replace_private(path: &Path, bytes: &[u8]) -> Result<()> {
    create_parent(path)?;
    let (temporary, mut file) = create_private_temp(path)?;
    let write_result = (|| -> io::Result<()> {
        file.write_all(bytes)?;
        file.flush()
    })();
    drop(file);
    if let Err(error) = write_result {
        fs::remove_file(&temporary).ok();
        return Err(error).with_context(|| format!("write {}", temporary.display()));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        fs::remove_file(&temporary).ok();
        return Err(error).with_context(|| format!("replace {}", path.display()));
    }
    Ok(())
}

fn create_private_temp(path: &Path) -> Result<(PathBuf, File)> {
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("state file has no file name"))?
        .to_string_lossy();
    for _ in 0..16 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = path.with_file_name(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("create {}", temporary.display()));
            }
        }
    }
    Err(anyhow!("could not allocate a private state tempfile"))
}

fn create_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    Ok(())
}

/// Seconds since the Unix epoch, or 0 if the clock is before it.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    use super::*;

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "switchboard-state-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("test clock is after epoch")
                    .as_nanos()
            ))
            .join(name)
    }

    #[test]
    fn concurrent_private_updates_are_serialized_without_losing_data() {
        let path = test_path("counter");
        let ready = Arc::new(Barrier::new(9));
        let mut workers = Vec::new();

        for _ in 0..8 {
            let path = path.clone();
            let ready = Arc::clone(&ready);
            workers.push(thread::spawn(move || {
                ready.wait();
                update_private(&path, |current| {
                    let value = current
                        .and_then(|bytes| std::str::from_utf8(bytes).ok())
                        .and_then(|text| text.parse::<u64>().ok())
                        .unwrap_or(0);
                    // Widen the read/write window so the test fails reliably if
                    // the transaction lock is removed.
                    thread::sleep(Duration::from_millis(2));
                    let next = value + 1;
                    Ok((next.to_string().into_bytes(), next))
                })
                .unwrap()
            }));
        }

        ready.wait();
        let mut observed = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        observed.sort_unstable();
        assert_eq!(observed, (1..=8).collect::<Vec<_>>());
        assert_eq!(fs::read_to_string(&path).unwrap(), "8");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let parent = path.parent().unwrap();
        assert!(fs::read_dir(parent).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
        fs::remove_dir_all(parent).ok();
    }
}
