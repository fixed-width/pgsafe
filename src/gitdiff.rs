//! Isolated git interaction for `--git-diff` mode: the `.sql` files changed versus a ref.
//! ALL `git` invocation in pgsafe lives here — nothing else shells out to git. Built only
//! with the `cli` feature.

use std::path::{Path, PathBuf};
use std::process::Command;

/// List the `.sql` files added/modified versus `reference`, optionally narrowed to the
/// `scope` git pathspecs. Returns **absolute** paths the caller can read directly (without
/// knowing the repo root). Any git failure becomes an `Err(message)`.
pub(crate) fn changed_sql_files(reference: &str, scope: &[String]) -> Result<Vec<PathBuf>, String> {
    let root = repo_root()?;
    let names = changed_names(reference, scope)?;
    Ok(select_sql(&root, names))
}

/// Keep only `.sql` names (case-insensitive) and resolve each against the repo root.
/// Pure — no git — so the filtering/joining is unit-testable on its own.
fn select_sql(root: &Path, names: Vec<String>) -> Vec<PathBuf> {
    names
        .into_iter()
        .filter(|n| {
            Path::new(n)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("sql"))
        })
        .map(|n| root.join(n))
        .collect()
}

/// Repo root via `git rev-parse --show-toplevel` (also confirms we are inside a repo).
fn repo_root() -> Result<PathBuf, String> {
    let out = run_git(&["rev-parse", "--show-toplevel"])?;
    let path = out.trim();
    if path.is_empty() {
        return Err("not a git repository".to_string());
    }
    Ok(PathBuf::from(path))
}

/// Changed file names (repo-root-relative) for `reference`, scoped by `scope` pathspecs.
///
/// Collects both tracked changes (`git diff --name-only`) and untracked files
/// (`git ls-files --others`) so that newly written migrations are picked up
/// even before `git add`.
fn changed_names(reference: &str, scope: &[String]) -> Result<Vec<String>, String> {
    // Tracked: added/copied/modified/renamed vs the reference (staged or unstaged).
    let mut diff_args = vec!["diff", "--name-only", "--diff-filter=ACMR", reference];
    if !scope.is_empty() {
        diff_args.push("--");
        diff_args.extend(scope.iter().map(String::as_str));
    }
    let diff_out = run_git(&diff_args)?;

    // Untracked: files not yet known to git (respects .gitignore).
    let mut ls_args = vec!["ls-files", "--others", "--exclude-standard"];
    if !scope.is_empty() {
        ls_args.push("--");
        ls_args.extend(scope.iter().map(String::as_str));
    }
    let ls_out = run_git(&ls_args)?;

    let mut names: Vec<String> = diff_out
        .lines()
        .chain(ls_out.lines())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_owned())
        .collect();
    names.sort_unstable();
    names.dedup();
    Ok(names)
}

/// Run `git <args>`, returning stdout on success or a message on failure.
fn run_git(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let trimmed = stderr.trim();
        return Err(if trimmed.is_empty() {
            "git command failed".to_string()
        } else {
            trimmed.to_string()
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_sql_keeps_only_sql_and_joins_to_root() {
        let root = Path::new("/repo");
        let got = select_sql(
            root,
            vec![
                "db/a.sql".to_string(),
                "README.md".to_string(),
                "db/b.SQL".to_string(), // case-insensitive
                "c.sql".to_string(),
                "d".to_string(), // no extension
            ],
        );
        assert_eq!(
            got,
            vec![
                PathBuf::from("/repo/db/a.sql"),
                PathBuf::from("/repo/db/b.SQL"),
                PathBuf::from("/repo/c.sql"),
            ]
        );
    }
}
