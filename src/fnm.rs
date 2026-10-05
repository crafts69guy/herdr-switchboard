//! Optional fnm integration for project opens.
//!
//! The interface has two paths: [`inspect`] is filesystem-only and cheap enough
//! for repository previews, while [`prepare`] asks fnm for an installed version's
//! PATH only after the user chooses to open a project. Nothing here installs a
//! Node version or performs network work.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::runner::CommandRunner;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Declaration {
    pub requested: String,
    pub source: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Preparation {
    /// The project declares no Node version.
    Unmanaged,
    /// fnm is not installed or is not reachable from Switchboard's PATH.
    FnmMissing,
    /// fnm found an installed match and returned the PATH for it.
    Ready { path: String },
    /// A declaration exists, but fnm could not resolve it to an installed version.
    Unavailable,
}

/// The project Node declaration fnm would consider, without spawning a process.
pub fn inspect(path: &str) -> Option<Declaration> {
    let recursive = env::var("FNM_VERSION_FILE_STRATEGY").as_deref() == Ok("recursive");
    let resolve_engines = env::var("FNM_RESOLVE_ENGINES")
        .map(|value| value != "false")
        .unwrap_or(true);
    inspect_with(Path::new(path), recursive, resolve_engines)
}

/// Resolve the project's already-installed Node version to a launch PATH.
pub fn prepare(runner: &dyn CommandRunner, path: &str) -> Preparation {
    if inspect(path).is_none() {
        return Preparation::Unmanaged;
    }

    let output = match runner.output("fnm", &["exec", "--using", path, "--", "printenv", "PATH"]) {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Preparation::FnmMissing;
        }
        Err(_) => return Preparation::Unavailable,
    };
    if !output.status.success() {
        return Preparation::Unavailable;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        Preparation::Unavailable
    } else {
        Preparation::Ready { path }
    }
}

fn inspect_with(path: &Path, recursive: bool, resolve_engines: bool) -> Option<Declaration> {
    let mut dir = Some(path);
    while let Some(current) = dir {
        if let Some(declaration) = inspect_dir(current, resolve_engines) {
            return Some(declaration);
        }
        if !recursive {
            break;
        }
        dir = current.parent();
    }
    None
}

fn inspect_dir(dir: &Path, resolve_engines: bool) -> Option<Declaration> {
    for (name, source) in [(".nvmrc", ".nvmrc"), (".node-version", ".node-version")] {
        if let Some(requested) = read_trimmed(dir.join(name)) {
            return Some(Declaration { requested, source });
        }
    }
    if resolve_engines {
        package_engine(dir.join("package.json")).map(|requested| Declaration {
            requested,
            source: "package.json",
        })
    } else {
        None
    }
}

fn read_trimmed(path: PathBuf) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn package_engine(path: PathBuf) -> Option<String> {
    let value: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    value["engines"]["node"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;
    use std::process::{ExitStatus, Output};

    fn temp_dir(tag: &str) -> PathBuf {
        let path = env::temp_dir().join(format!("switchboard-fnm-{tag}-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn declaration_precedence_matches_fnm() {
        let dir = temp_dir("precedence");
        fs::write(dir.join("package.json"), r#"{"engines":{"node":"20"}}"#).unwrap();
        fs::write(dir.join(".node-version"), "22\n").unwrap();
        fs::write(dir.join(".nvmrc"), "24\n").unwrap();

        assert_eq!(
            inspect_with(&dir, false, true),
            Some(Declaration {
                requested: "24".into(),
                source: ".nvmrc"
            })
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn engine_lookup_can_be_disabled() {
        let dir = temp_dir("engines-off");
        fs::write(
            dir.join("package.json"),
            r#"{"engines":{"node":">=20 <21"}}"#,
        )
        .unwrap();

        assert_eq!(inspect_with(&dir, false, false), None);
        assert_eq!(
            inspect_with(&dir, false, true),
            Some(Declaration {
                requested: ">=20 <21".into(),
                source: "package.json"
            })
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn recursive_lookup_walks_to_a_parent() {
        let dir = temp_dir("recursive");
        let child = dir.join("packages/app");
        fs::create_dir_all(&child).unwrap();
        fs::write(dir.join(".node-version"), "22.16.0\n").unwrap();

        assert_eq!(inspect_with(&child, false, true), None);
        assert_eq!(
            inspect_with(&child, true, true).map(|value| value.requested),
            Some("22.16.0".into())
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn malformed_package_json_is_unmanaged() {
        let dir = temp_dir("malformed");
        fs::write(dir.join("package.json"), "{not json").unwrap();
        assert_eq!(inspect_with(&dir, false, true), None);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn prepare_returns_the_path_from_fnm_exec() {
        let dir = temp_dir("ready");
        fs::write(dir.join(".nvmrc"), "22\n").unwrap();
        let path = dir.to_string_lossy().to_string();
        let runner = MockRunner::new().on("fnm exec", "/fnm/v22/bin:/usr/bin\n");

        assert_eq!(
            prepare(&runner, &path),
            Preparation::Ready {
                path: "/fnm/v22/bin:/usr/bin".into()
            }
        );
        assert_eq!(
            runner.calls()[0],
            vec!["fnm", "exec", "--using", &path, "--", "printenv", "PATH"]
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unmanaged_projects_do_not_spawn_fnm() {
        let dir = temp_dir("unmanaged");
        let path = dir.to_string_lossy().to_string();
        let runner = MockRunner::new();
        assert_eq!(prepare(&runner, &path), Preparation::Unmanaged);
        assert!(runner.calls().is_empty());
        fs::remove_dir_all(dir).ok();
    }

    /// A runner whose every spawn fails with `kind`: `NotFound` is a missing
    /// binary, anything else a spawn that failed for another reason.
    struct FailingSpawn(std::io::ErrorKind);

    impl CommandRunner for FailingSpawn {
        fn output(&self, _program: &str, _args: &[&str]) -> std::io::Result<Output> {
            Err(std::io::Error::from(self.0))
        }

        fn status(&self, _program: &str, _args: &[&str]) -> std::io::Result<ExitStatus> {
            Err(std::io::Error::from(self.0))
        }

        fn output_stdin(
            &self,
            _program: &str,
            _args: &[&str],
            _stdin: &str,
        ) -> std::io::Result<Output> {
            Err(std::io::Error::from(self.0))
        }

        fn spawn_detached(
            &self,
            _program: &std::ffi::OsStr,
            _args: &[&str],
        ) -> std::io::Result<()> {
            Err(std::io::Error::from(self.0))
        }
    }

    #[test]
    fn a_missing_fnm_binary_is_distinct_from_a_missing_version() {
        let dir = temp_dir("missing-fnm");
        fs::write(dir.join(".nvmrc"), "22\n").unwrap();
        let path = dir.to_string_lossy().to_string();
        let missing = FailingSpawn(std::io::ErrorKind::NotFound);
        assert_eq!(prepare(&missing, &path), Preparation::FnmMissing);
        let denied = FailingSpawn(std::io::ErrorKind::PermissionDenied);
        assert_eq!(prepare(&denied, &path), Preparation::Unavailable);
        assert!(denied.status("fnm", &[]).is_err());
        assert!(denied.output_stdin("fnm", &[], "").is_err());
        assert!(denied
            .spawn_detached(std::ffi::OsStr::new("fnm"), &[])
            .is_err());

        let runner = MockRunner::new().failing("fnm exec");
        assert_eq!(prepare(&runner, &path), Preparation::Unavailable);
        // fnm answered but printed no PATH: nothing to launch with.
        let silent = MockRunner::new().on("fnm exec", "  \n");
        assert_eq!(prepare(&silent, &path), Preparation::Unavailable);
        fs::remove_dir_all(dir).ok();
    }
}
