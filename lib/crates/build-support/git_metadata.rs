use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Eq, PartialEq)]
pub struct BuildGitMetadata {
    pub rerun_paths: Vec<PathBuf>,
    pub short_sha: String,
}

pub fn collect_from(package_dir: &Path) -> BuildGitMetadata {
    let mut rerun_paths = Vec::new();

    if let Some(head_path) = git_output(package_dir, ["rev-parse", "--git-path", "HEAD"]) {
        rerun_paths.push(PathBuf::from(head_path));
    }

    if let Some(head_ref) = git_output(package_dir, ["symbolic-ref", "-q", "HEAD"]) {
        if let Some(ref_path) = git_output(package_dir, ["rev-parse", "--git-path", &head_ref]) {
            rerun_paths.push(PathBuf::from(ref_path));
        }
    }

    let short_sha = git_output(package_dir, ["rev-list", "-1", "HEAD"])
        .map(|sha| {
            if sha.len() >= 7 {
                sha[..7].to_string()
            } else {
                sha
            }
        })
        .unwrap_or_default();

    BuildGitMetadata {
        rerun_paths,
        short_sha,
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Build scripts read Cargo-provided PROFILE outside application runtime configuration."
)]
pub fn cargo_profile() -> String {
    std::env::var("PROFILE").unwrap_or_default()
}

/// True when the working tree at `package_dir` has uncommitted changes, so a
/// build from it is not reproducible from HEAD alone. Gitignored files are
/// excluded by git; any other tracked or untracked change counts.
#[expect(
    clippy::disallowed_methods,
    reason = "Build tooling probes git synchronously for the dirty flag."
)]
pub fn is_dirty(package_dir: &Path) -> bool {
    Command::new("git")
        .current_dir(package_dir)
        .args(["status", "--porcelain"])
        .output()
        .map(|output| output.status.success() && !output.stdout.is_empty())
        .unwrap_or(false)
}

#[expect(
    clippy::disallowed_methods,
    reason = "Build scripts run outside Tokio and need synchronous git probes for embedded build metadata."
)]
fn git_output<const N: usize>(package_dir: &Path, args: [&str; N]) -> Option<String> {
    Command::new("git")
        .current_dir(package_dir)
        .args(args)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|output| output.trim().to_string())
            } else {
                None
            }
        })
        .filter(|output| !output.is_empty())
}
