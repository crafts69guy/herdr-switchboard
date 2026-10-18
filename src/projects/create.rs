//! Safe creation of a new local Git repository under ghq's current root.

use std::fs;
use std::path::Path;

use anyhow::{anyhow, ensure, Context, Result};

use crate::runner::CommandRunner;

/// Resolves the form input to a path relative to ghq root: a bare name lands
/// under `local/`, while `a/b/name` is taken as typed. Every component must be
/// a plain directory name, so the result can never leave the root.
pub(super) fn path(input: &str) -> Result<String> {
    let path = input.trim();
    let components: Vec<&str> = path.split('/').collect();
    ensure!(
        !input.chars().any(|c| c == '\\' || c.is_control())
            && components
                .iter()
                .all(|c| !c.is_empty() && !matches!(*c, "." | ".." | ".git")),
        "Enter a name or a relative path like owner/repo, without '.', '..', or control characters."
    );
    Ok(if components.len() == 1 {
        format!("local/{path}")
    } else {
        path.to_owned()
    })
}

pub(super) fn root(runner: &dyn CommandRunner) -> Result<String> {
    let root = runner
        .capture("ghq", &["root"])
        .ok_or_else(|| anyhow!("Could not read ghq root. Check ghq configuration and retry."))?;
    ensure!(
        Path::new(&root).is_absolute() && Path::new(&root).is_dir(),
        "ghq root must be an existing absolute directory."
    );
    let root = fs::canonicalize(root).context("Could not resolve ghq root.")?;
    root.to_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("ghq root must be a valid UTF-8 path."))
}

pub(super) fn run(runner: &dyn CommandRunner, root: &str, input: &str) -> Result<(String, String)> {
    let relative = path(input)?;
    let root = Path::new(root);
    ensure!(
        root.is_absolute() && root.is_dir(),
        "ghq root is no longer available."
    );
    let (parents, name) = relative.rsplit_once('/').unwrap_or(("", &relative));
    let mut parent = root.to_path_buf();
    for component in parents.split('/') {
        parent.push(component);
        match fs::create_dir(&parent) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("Could not create a parent directory."),
        }
        let metadata =
            fs::symlink_metadata(&parent).context("Could not inspect a parent directory.")?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Every parent directory must be a directory, not a symlink."
        );
        ensure!(
            !parent.join(".git").exists(),
            "That path is inside an existing repository. Choose another path."
        );
    }
    let path = parent.join(name);
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("Repository path must be valid UTF-8."))?;
    // create_dir is exclusive: an existing checkout must never be re-initialized.
    fs::create_dir(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            anyhow!("That repository path already exists. Choose another name.")
        } else {
            anyhow!("Could not create the repository directory: {error}")
        }
    })?;
    let output = runner.output("git", &["-C", path, "init"]).map_err(|_| {
        anyhow!("Could not start Git. Install Git and retry with another name. The created directory was kept.")
    })?;
    ensure!(
        output.status.success(),
        "Git initialization failed. The created directory was kept; inspect it before retrying."
    );
    Ok((relative, path.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let root = crate::state::test_scratch()
            .unwrap()
            .join(format!("create-{tag}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn bare_names_land_under_local_and_relative_paths_stay_inside_the_root() {
        for input in [
            "", " ", ".", "..", ".git", "a\\b", "../repo", "a/../b", "a/./b", "/abs", "a//b", "a/",
            "a/.git", "repo\n", "\trepo", "a\0b",
        ] {
            assert!(path(input).is_err(), "{input:?}");
        }
        for input in ["my-repo", "repo.v2", "dự án", "-option", "  repo  "] {
            assert_eq!(path(input).unwrap(), format!("local/{}", input.trim()));
        }
        for input in ["work/repo", "github.com/me/dự án", " local/x "] {
            assert_eq!(path(input).unwrap(), input.trim());
        }
    }

    #[test]
    fn ghq_root_must_exist_and_resolves_configured_symlinks() {
        let dir = scratch("root");
        let alias = dir.join("alias");
        let target = dir.join("target");
        fs::create_dir(&target).unwrap();
        symlink(&target, &alias).unwrap();
        assert_eq!(
            root(&MockRunner::new().on("ghq root", alias.to_str().unwrap())).unwrap(),
            target.canonicalize().unwrap().to_str().unwrap()
        );
        for value in ["", "relative", "/this-directory-does-not-exist"] {
            assert!(root(&MockRunner::new().on("ghq root", value)).is_err());
        }
        assert!(root(&MockRunner::new().failing("ghq root")).is_err());
    }

    #[test]
    fn creation_is_exclusive_and_passes_names_as_one_git_argument() {
        let dir = scratch("exclusive");
        let runner = MockRunner::new();
        let (id, path) = run(&runner, dir.to_str().unwrap(), "  dự án  ").unwrap();
        assert_eq!(id, "local/dự án");
        assert_eq!(Path::new(&path), dir.join("local/dự án"));
        assert_eq!(runner.calls(), [vec!["git", "-C", path.as_str(), "init"]]);
        fs::write(Path::new(&path).join("keep.txt"), "existing data").unwrap();
        assert!(run(&runner, dir.to_str().unwrap(), "dự án").is_err());
        assert_eq!(
            fs::read_to_string(Path::new(&path).join("keep.txt")).unwrap(),
            "existing data"
        );
        assert_eq!(runner.calls().len(), 1);
        run(&runner, dir.to_str().unwrap(), "-option").unwrap();
        assert!(run(&runner, "relative", "repo").is_err());
        assert!(run(&runner, "/this-directory-does-not-exist", "repo").is_err());
        assert!(run(&runner, dir.to_str().unwrap(), "../outside").is_err());
    }

    #[test]
    fn local_and_repository_symlinks_and_existing_files_are_refused() {
        let dir = scratch("symlinks");
        let outside = dir.join("outside");
        fs::create_dir(&outside).unwrap();
        let local = dir.join("local");
        symlink(&outside, &local).unwrap();
        let runner = MockRunner::new();
        assert!(run(&runner, dir.to_str().unwrap(), "repo").is_err());
        assert!(!outside.join("repo").exists());
        fs::remove_file(&local).unwrap();
        fs::write(&local, "not a directory").unwrap();
        assert!(run(&runner, dir.to_str().unwrap(), "repo").is_err());
        fs::remove_file(&local).unwrap();
        fs::create_dir(&local).unwrap();
        symlink(&outside, local.join("repo")).unwrap();
        assert!(run(&runner, dir.to_str().unwrap(), "repo").is_err());
        fs::write(local.join("existing"), "keep").unwrap();
        assert!(run(&runner, dir.to_str().unwrap(), "existing").is_err());
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn failed_git_keeps_the_directory_and_hides_external_diagnostics() {
        let dir = scratch("git-failed");
        let runner = MockRunner::new().failing_with("git", "private diagnostic");
        let error = run(&runner, dir.to_str().unwrap(), "repo")
            .unwrap_err()
            .to_string();
        assert!(error.contains("directory was kept"));
        assert!(!error.contains("private diagnostic"));
        assert!(dir.join("local/repo").is_dir());
    }

    #[test]
    fn relative_paths_create_missing_parents_but_never_nest_or_follow_symlinks() {
        let dir = scratch("relative");
        let runner = MockRunner::new();
        let (id, path) = run(&runner, dir.to_str().unwrap(), "github.com/me/new").unwrap();
        assert_eq!(id, "github.com/me/new");
        assert_eq!(Path::new(&path), dir.join("github.com/me/new"));
        fs::create_dir(dir.join("github.com/me/new/.git")).unwrap();
        assert!(run(&runner, dir.to_str().unwrap(), "github.com/me/new/sub").is_err());
        assert!(!dir.join("github.com/me/new/sub").exists());
        let outside = dir.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, dir.join("linked")).unwrap();
        assert!(run(&runner, dir.to_str().unwrap(), "linked/deep/repo").is_err());
        assert!(!outside.join("deep").exists());
        assert_eq!(runner.calls().len(), 1);
    }
}
