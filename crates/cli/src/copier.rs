//! `copier update` for one template, in the user's own working tree, with
//! the user's own `copier`, environment and permissions. Only the working-tree
//! TUI calls this, after its own confirmation; lookups, runs, the server and
//! `--remote` never reach it. Copier inherits the terminal so it can ask its own
//! questions. The Count never passes `--trust` and never commits.
use std::{path::Path, process::Command};

#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    /// Every path copier changed, created or deleted, relative to the tree.
    pub touched: Vec<String>,
    /// Touched files holding conflict markers, and `.rej` files.
    pub conflicts: Vec<String>,
}

/// Paths `git status` reports in `dir`, untracked files included.
fn changed(dir: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("copier needs git to check the working tree: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "copier needs a git checkout: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut paths = Vec::new();
    let mut entries = text.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        let Some(path) = entry.get(3..) else {
            continue;
        };
        paths.push(path.to_owned());
        // A rename is followed by its old path.
        if entry.starts_with('R') || entry.starts_with('C') {
            entries.next();
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Run `program update` for the answers file `answers` (relative to `dir`),
/// moving its template to `tag`. The caller must hold a terminal.
pub fn update(program: &str, dir: &Path, answers: &str, tag: &str) -> Result<Outcome, String> {
    if !changed(dir)?.is_empty() {
        return Err(
            "the working tree has uncommitted changes, and copier needs a clean tree; commit or stash them first".into(),
        );
    }
    let answers = Path::new(answers);
    let destination = answers
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file = answers.file_name().ok_or("invalid answers file path")?;
    let status = Command::new(program)
        .arg("update")
        .arg("--vcs-ref")
        .arg(tag)
        .arg("--skip-answered")
        .arg("--answers-file")
        .arg(file)
        .arg(destination)
        .current_dir(dir)
        .status()
        .map_err(|e| format!("cannot run {program}: {e}; install copier and try again"))?;
    let touched = changed(dir)?;
    if !status.success() {
        return Err(format!(
            "copier stopped ({status}) and {} file{} changed. If it asked for --trust, review the template and run copier update --trust yourself; the Count never trusts a template for you",
            touched.len(),
            if touched.len() == 1 { "" } else { "s" }
        ));
    }
    let conflicts = touched
        .iter()
        .filter(|path| {
            path.ends_with(".rej")
                || std::fs::read_to_string(dir.join(path))
                    .is_ok_and(|text| text.lines().any(|l| l.starts_with("<<<<<<< ")))
        })
        .cloned()
        .collect();
    Ok(Outcome { touched, conflicts })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
    /// A checkout with one answers file, committed, and a fake copier that
    /// records its arguments and runs `body`.
    fn checkout(body: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(tree.join("api")).unwrap();
        std::fs::write(tree.join("api/.copier-answers.yml"), "_commit: v1.4.0\n").unwrap();
        std::fs::write(tree.join("README.md"), "demo\n").unwrap();
        git(&tree, &["init", "-q"]);
        git(&tree, &["add", "."]);
        git(&tree, &["commit", "-qm", "init"]);
        let fake = dir.path().join("copier");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho \"$PWD $*\" > ../args\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, fake.to_string_lossy().into_owned())
    }

    #[test]
    fn copier_runs_for_the_template_and_reports_touched_files_and_conflicts() {
        let (dir, fake) = checkout(
            "printf '_commit: v1.6.0\\n' > api/.copier-answers.yml\nprintf '<<<<<<< before updating\\nold\\n=======\\nnew\\n>>>>>>> after updating\\n' > README.md\necho rejected > api/ci.yml.rej\nexit 0",
        );
        let tree = dir.path().join("tree");
        let outcome = update(&fake, &tree, "api/.copier-answers.yml", "v1.6.0").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("args")).unwrap(),
            format!(
                "{} update --vcs-ref v1.6.0 --skip-answered --answers-file .copier-answers.yml api\n",
                tree.canonicalize().unwrap().display()
            )
        );
        assert_eq!(
            outcome,
            Outcome {
                touched: vec![
                    "README.md".into(),
                    "api/.copier-answers.yml".into(),
                    "api/ci.yml.rej".into()
                ],
                conflicts: vec!["README.md".into(), "api/ci.yml.rej".into()],
            }
        );
    }

    #[test]
    fn a_dirty_tree_is_refused_before_copier_runs() {
        let (dir, fake) = checkout("exit 0");
        let tree = dir.path().join("tree");
        std::fs::write(tree.join("notes.txt"), "draft\n").unwrap();
        let error = update(&fake, &tree, "api/.copier-answers.yml", "v1.6.0").unwrap_err();
        assert!(error.contains("uncommitted changes"), "{error}");
        assert!(!dir.path().join("args").exists());
    }

    #[test]
    fn a_failed_copier_says_how_to_trust_the_template_yourself() {
        let (dir, fake) =
            checkout("echo 'Template uses potentially unsafe feature: tasks.' >&2\nexit 1");
        let tree = dir.path().join("tree");
        let error = update(&fake, &tree, "api/.copier-answers.yml", "v1.6.0").unwrap_err();
        assert!(error.contains("0 files changed"), "{error}");
        assert!(error.contains("copier update --trust yourself"), "{error}");
    }
}
