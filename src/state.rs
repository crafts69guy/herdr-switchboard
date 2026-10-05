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
    #[cfg(test)]
    if let Some(scratch) = test_scratch() {
        return Some(scratch.join("state"));
    }
    state_dir_from(env::var("XDG_STATE_HOME").ok(), env::var("HOME").ok())
}

fn state_dir_from(xdg_state: Option<String>, home: Option<String>) -> Option<PathBuf> {
    let base = xdg_state
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("herdr-switchboard"))
}

/// Where the test build keeps everything that would otherwise be the user's:
/// this plugin's state, its config, herdr's config, and herdr's socket. One
/// directory per test process, so no test can read or write the real ones —
/// a background step that once reached `review_archive::set` from a test wrote
/// a fixture slug into the developer's own archive.
#[cfg(test)]
pub(crate) fn test_scratch() -> Option<PathBuf> {
    Some(std::env::temp_dir().join(format!("swb-test-{}", std::process::id())))
}

/// A file inside [`state_dir`], or `None` when there is no state dir.
pub fn state_file(name: &str) -> Option<PathBuf> {
    Some(state_dir()?.join(name))
}

/// Who may read the file this module leaves behind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    /// Owner-only from creation. Correct for everything under [`state_dir`],
    /// which is this plugin's private bookkeeping.
    Private,
    /// Whatever the replaced file already carried, or the process default when
    /// there is nothing to replace. Correct for a *configuration* file, which
    /// belongs to the user rather than to this plugin: gaining atomicity and a
    /// lock must not also silently re-permission a file they own.
    Inherited,
}

/// Replace one state file atomically with bytes that are private from creation.
///
/// A persistent sibling lock serializes writers across Switchboard processes;
/// a unique sibling tempfile keeps each replacement isolated until rename.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let _lock = acquire_private_lock(path)?;
    replace(path, bytes, Access::Private)
}

/// Replace one file atomically, keeping the permissions it already had.
///
/// The same lock and unique tempfile as [`write_private`], for the files that
/// are *not* this plugin's private state: its own `config.toml` and — for zen
/// chrome — herdr's. They need the atomicity and the cross-process lock just as
/// much, since two panes can apply settings or enter zen at the same moment, but
/// they must keep the mode their owner gave them.
pub fn replace_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let _lock = acquire_private_lock(path)?;
    replace(path, bytes, Access::Inherited)
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
    replace(path, &replacement, Access::Private)?;
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

fn replace(path: &Path, bytes: &[u8], access: Access) -> Result<()> {
    create_parent(path)?;
    // Written owner-only whatever the eventual mode, so no window exists in
    // which the half-written replacement is readable by anyone else.
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
    if access == Access::Inherited {
        if let Err(error) = inherit_permissions(path, &temporary) {
            fs::remove_file(&temporary).ok();
            return Err(error);
        }
    }
    if let Err(error) = fs::rename(&temporary, path) {
        fs::remove_file(&temporary).ok();
        return Err(error).with_context(|| format!("replace {}", path.display()));
    }
    Ok(())
}

/// Give `temporary` the mode `path` already has, or the ordinary
/// create-a-new-file mode when `path` does not exist yet.
///
/// A configuration file that was group- or world-readable stays that way; one
/// the user had locked down stays locked down. Without this, gaining an atomic
/// replace would quietly narrow the file to owner-only on its next write.
///
/// When there is nothing to inherit from, the target is created empty first and
/// its mode read back. That is a deliberately roundabout way to ask "what would
/// an ordinary create have produced here", but it is the only one that does not
/// need the process umask — which libc will only reveal by setting it, a swap
/// that is unsafe to perform in a threaded process. Holding the lock, and
/// renaming over it immediately, keeps the empty file from ever being observed.
fn inherit_permissions(path: &Path, temporary: &Path) -> Result<()> {
    let existing = match fs::metadata(path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            File::create(path).with_context(|| format!("create {}", path.display()))?;
            fs::metadata(path).with_context(|| format!("read mode of {}", path.display()))?
        }
        Err(error) => {
            return Err(error).with_context(|| format!("read permissions of {}", path.display()));
        }
    };
    fs::set_permissions(temporary, existing.permissions())
        .with_context(|| format!("copy permissions onto {}", temporary.display()))
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

    /// A path no other test can collide with.
    ///
    /// The nanosecond clock alone was not enough: tests run in parallel threads
    /// and macOS does not hand out a distinct nanosecond per call, so two
    /// fixtures could land on one directory — and one of these tests puts a
    /// *directory* where another expects a file.
    fn test_path(name: &str) -> PathBuf {
        static NONCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir()
            .join(format!(
                "switchboard-state-{}-{}",
                std::process::id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
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

    /// A configuration file belongs to the user, not to this plugin. Routing it
    /// through here buys atomicity and a lock; it must not also re-permission
    /// the file, in either direction.
    #[cfg(unix)]
    #[test]
    fn an_atomic_replace_keeps_the_permissions_the_file_already_had() {
        use std::os::unix::fs::PermissionsExt;

        let path = test_path("config.toml");
        create_parent(&path).unwrap();
        for mode in [0o644, 0o600, 0o664] {
            fs::write(&path, b"before").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();

            replace_atomically(&path, b"after").unwrap();

            assert_eq!(fs::read(&path).unwrap(), b"after");
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                mode,
                "an atomic replace changed the file's mode"
            );
        }
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// With nothing to inherit from, a new configuration file must land the way
    /// an ordinary create would — never left at the tempfile's owner-only mode,
    /// which is what a state file gets and a config file must not.
    #[cfg(unix)]
    #[test]
    fn a_new_configuration_file_lands_at_the_ordinary_create_mode() {
        use std::os::unix::fs::PermissionsExt;

        let reference = test_path("reference.toml");
        create_parent(&reference).unwrap();
        fs::write(&reference, b"x").unwrap();
        let expected = fs::metadata(&reference).unwrap().permissions().mode() & 0o777;

        let path = reference.with_file_name("fresh.toml");
        replace_atomically(&path, b"new").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            expected
        );
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// State files keep the opposite guarantee: owner-only, whatever mode the
    /// file being replaced happened to carry.
    #[cfg(unix)]
    #[test]
    fn a_private_write_narrows_a_permissive_state_file() {
        use std::os::unix::fs::PermissionsExt;

        let path = test_path("cache.tsv");
        create_parent(&path).unwrap();
        fs::write(&path, b"before").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        write_private(&path, b"after").unwrap();

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// Every refusal leaves nothing behind: replacing a directory, a path with
    /// no file name, and a target whose temp file cannot be created.
    #[test]
    fn a_refused_write_leaves_no_temp_file_behind() {
        let dir = std::env::temp_dir().join(format!("swb-state-refusals-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("occupied/inner")).unwrap();

        let error = write_private(&dir.join("occupied"), b"x").unwrap_err();
        assert!(error.to_string().contains("replace"), "{error}");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        assert!(replace_atomically(Path::new("/"), b"x").is_err());
        // The parent is a file, so nothing can be created beneath it.
        fs::write(dir.join("plain"), b"").unwrap();
        assert!(write_private(&dir.join("plain/child.tsv"), b"x").is_err());
        fs::remove_dir_all(&dir).ok();
    }

    /// Outside the test build the state lives under XDG, then HOME, and
    /// nowhere when neither is set; inside it, under the per-process scratch.
    #[test]
    fn state_lives_under_xdg_then_home_and_tests_use_scratch() {
        assert_eq!(
            state_dir_from(Some("/x".into()), Some("/home/u".into())),
            Some(PathBuf::from("/x/herdr-switchboard"))
        );
        assert_eq!(
            state_dir_from(Some(String::new()), Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.local/state/herdr-switchboard"))
        );
        assert_eq!(state_dir_from(None, None), None);
        assert!(state_dir().unwrap().starts_with(test_scratch().unwrap()));
    }
}
