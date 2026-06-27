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
    let names = changed_names(&root, reference, scope)?;
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

/// Repo root via `git rev-parse --show-toplevel` (run in the caller's CWD; also confirms a repo).
fn repo_root() -> Result<PathBuf, String> {
    let out = run_git(None, &["rev-parse", "--show-toplevel"])?;
    let path = out.trim();
    if path.is_empty() {
        return Err("not a git repository".to_string());
    }
    Ok(PathBuf::from(path))
}

/// Changed (tracked, vs `reference`) plus untracked file names, repo-root-relative.
/// Both git commands run anchored at `root` so their paths share one namespace and the
/// result is correct regardless of the caller's working directory.
fn changed_names(root: &Path, reference: &str, scope: &[String]) -> Result<Vec<String>, String> {
    // `-z` makes git NUL-terminate names and never quote them, so paths with spaces or
    // non-ASCII characters come through verbatim instead of as `"db/mig\303\251.sql"`.
    let mut diff_args = vec!["diff", "--name-only", "-z", "--diff-filter=ACMR", reference];
    if !scope.is_empty() {
        diff_args.push("--");
        diff_args.extend(scope.iter().map(String::as_str));
    }
    let diff_out = run_git(Some(root), &diff_args).map_err(|e| {
        format!(
            "{e}\nhint: make sure `{reference}` is available — fetch it shallowly, e.g. \
             `git fetch --depth=1 origin <branch>` (full history is not required)"
        )
    })?;

    let mut ls_args = vec!["ls-files", "--others", "--exclude-standard", "-z"];
    if !scope.is_empty() {
        ls_args.push("--");
        ls_args.extend(scope.iter().map(String::as_str));
    }
    let ls_out = run_git(Some(root), &ls_args)?;

    // `-z` output is NUL-separated (with a trailing NUL), so split on '\0', not lines.
    let mut names: Vec<String> = diff_out
        .split('\0')
        .chain(ls_out.split('\0'))
        .filter(|l| !l.is_empty())
        .map(|l| l.to_owned())
        .collect();
    names.sort_unstable();
    names.dedup();
    Ok(names)
}

/// Run `git <args>`, optionally anchored at `dir`; stdout on success, a message on failure.
fn run_git(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let output = cmd
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
