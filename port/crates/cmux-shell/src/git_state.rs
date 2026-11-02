//! Design lane 3 git metadata (feature-parity row 11): the sub-header git
//! and diff chips for the focused pane's repository. `GitMeta` is the
//! sampled truth; parsing is pure and unit-tested, sampling shells out to
//! the installed `git.exe` with fixed read-only arguments.
//!
//! Working-directory honesty: the port does not track a live shell cwd
//! (no OSC 7 pipeline yet), so the watched directory is the pane's spawn
//! cwd — the process working directory that ConPTY hands the shell. A
//! later slice can widen this to per-pane live tracking without touching
//! the parsers.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Parsed repository state for one `-C <cwd>` sample.
///
/// `dirty` means uncommitted changes exist under HEAD (staged or
/// unstaged); `staged_adds`/`staged_dels` are the staged line totals the
/// design's `+12 -3 staged` chip shows. A never-initialized repository
/// parses no branch, so its badge hides instead of inventing a name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitMeta {
    pub branch: String,
    pub branch_detached: bool,
    pub dirty: bool,
    pub staged_adds: u64,
    pub staged_dels: u64,
}

impl GitMeta {
    /// The design's `[main*]` badge text: the asterisk marks a dirty
    /// worktree; a detached HEAD reads `[Detached HEAD]` literally.
    pub fn branch_badge(&self) -> Option<String> {
        if self.branch.is_empty() && !self.branch_detached {
            return None;
        }
        let marker = if self.dirty { "*" } else { "" };
        if self.branch_detached {
            Some("[Detached HEAD]".to_owned())
        } else {
            Some(format!("[{}{}]", self.branch, marker))
        }
    }
}

/// The git porcelain output of one sample, split on how the sampler ran.
/// `None` means the directory is not inside a repository (`rev-parse`
/// failed) or git is not runnable — chips hide.
pub fn sample_git(cwd: &Path) -> Option<GitMeta> {
    let status = run_git(cwd, ["status", "--porcelain", "-b"]);
    if status.trim().is_empty() && !is_repo(cwd) {
        return None;
    }
    let mut meta = parse_status_porcelain(&status);
    let numstat = run_git(cwd, ["diff", "--cached", "--numstat"]);
    let (adds, dels) = parse_numstat(&numstat);
    meta.staged_adds = adds;
    meta.staged_dels = dels;
    meta.dirty |= adds + dels > 0;
    Some(meta)
}

fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) -> String {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
}

fn is_repo(cwd: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Parses `git status --porcelain -b`: the leading `## ` branch line
/// carries the branch (and an optional `...origin/x` upstream), and any
/// following line marks a dirty worktree. `## No initial commit yet`
/// means an unborn HEAD — the port hides the badge rather than inventing
/// a branch name.
pub fn parse_status_porcelain(status: &str) -> GitMeta {
    let mut meta = GitMeta::default();
    let mut dirty = false;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if rest == "No initial commit yet" {
                meta.branch.clear();
                continue;
            }
            let branch = rest.split("...").next().unwrap_or(rest);
            if branch == "HEAD (no branch)" || branch.is_empty() {
                meta.branch_detached = true;
                continue;
            }
            meta.branch = branch.to_owned();
        } else if !line.trim().is_empty() {
            dirty = true;
        }
    }
    meta.dirty = dirty;
    meta
}

/// Sums staged `+ additions - deletions` line totals from
/// `git diff --cached --numstat`. Binary files print `-` for both
/// counts; those rows contribute nothing (line counts are undefined).
/// Malformed rows are skipped (the parser is monotone: no fabricated
/// totals).
pub fn parse_numstat(numstat: &str) -> (u64, u64) {
    let mut adds = 0;
    let mut dels = 0;
    for line in numstat.lines() {
        let mut fields = line.split('\t');
        let (Some(add_field), Some(del_field)) = (fields.next(), fields.next()) else {
            continue;
        };
        if let Ok(count) = add_field.parse::<u64>() {
            adds += count;
        }
        if let Ok(count) = del_field.parse::<u64>() {
            dels += count;
        }
    }
    (adds, dels)
}

/// The design abbreviates `C:\Users\noah\dev\x` as `~/dev/x`. Returns the
/// home-relative form of `cwd`, falling back to the absolute path.
pub fn abbreviate_home(cwd: &Path, home: Option<&str>) -> String {
    if let Some(home) = home.filter(|home| !home.is_empty()) {
        let home_path = PathBuf::from(home);
        if let Ok(rest) = cwd.strip_prefix(&home_path) {
            let rest = rest.to_string_lossy().replace('\\', "/");
            let rest = rest.strip_prefix('/').unwrap_or(&rest);
            if rest.is_empty() {
                return "~".to_owned();
            }
            return format!("~/{rest}");
        }
    }
    cwd.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_clean_branch_has_no_marker() {
        let meta = parse_status_porcelain("## main\n");
        assert_eq!(meta.branch, "main");
        assert!(!meta.dirty);
        assert_eq!(meta.branch_badge(), Some("[main]".to_owned()));
    }

    #[test]
    fn porcelain_dirty_line_marks_the_branch() {
        let meta = parse_status_porcelain("## dev...origin/dev\ndevpipeline.T2... M\n?? new\n");
        assert_eq!(meta.branch, "dev");
        assert!(meta.dirty);
        assert_eq!(meta.branch_badge(), Some("[dev*]".to_owned()));
    }

    #[test]
    fn porcelain_unborn_head_hides_the_badge() {
        // `git init` with no baseline and -b prefix handling: the branch
        // line stays "No initial commit yet" until the first commit.
        let meta = parse_status_porcelain("## No initial commit yet\n");
        assert!(meta.branch.is_empty());
        assert_eq!(meta.branch_badge(), None);
    }

    #[test]
    fn porcelain_detached_head_names_no_branch() {
        let meta = parse_status_porcelain("## HEAD (no branch)\nM  a.txt\n");
        assert!(meta.branch_detached);
        assert!(meta.dirty);
        assert_eq!(meta.branch_badge(), Some("[Detached HEAD]".to_owned()));
    }

    #[test]
    fn numstat_sums_staged_lines() {
        let (adds, dels) = parse_numstat("12\t3\tsrc/a.rs\n0\t4\tb.rs\n-\t-\tbin.c");
        assert_eq!(adds, 12);
        assert_eq!(dels, 7);
    }

    #[test]
    fn numstat_skips_malformed_rows() {
        assert_eq!(parse_numstat("garbage line\n\n"), (0, 0));
    }

    #[test]
    fn numstat_staged_state_is_the_chip_truth() {
        let meta = parse_status_porcelain("## main\n M x.txt\n");
        assert!(meta.dirty);
        // An empty cached numstat: no staged chip.
        assert_eq!(parse_numstat(""), (0, 0));
    }

    #[test]
    fn home_is_abbreviated_with_a_tilde() {
        let cwd = Path::new("C:\\Users\\noah\\dev\\orchestrator");
        assert_eq!(
            abbreviate_home(cwd, Some("C:\\Users\\noah")),
            "~/dev/orchestrator"
        );
        assert_eq!(abbreviate_home(cwd, None), "C:/Users/noah/dev/orchestrator");
        // Around the boundary: exactly the home directory and paths that
        // only share a prefix never rewrite.
        assert_eq!(
            abbreviate_home(Path::new("C:\\Users\\noah"), Some("C:\\Users\\noah")),
            "~"
        );
        assert_eq!(
            abbreviate_home(Path::new("C:\\Users\\noah2\\dev"), Some("C:\\Users\\noah")),
            "C:/Users/noah2/dev"
        );
    }

    #[test]
    fn sample_hides_until_a_repo_exists() {
        // The scratch parent is deliberately not a repository; the
        // sampler must not fabricate a branch (git 2.56: `status` fails
        // with empty stdout outside a repo).
        let scratch = std::env::temp_dir().join(format!("cmux-git-sampler-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        let found = sample_git(&scratch);
        let _ = std::fs::remove_dir_all(&scratch);
        assert!(
            found.is_none(),
            "a non-repository directory must hide the chips: {found:?}"
        );
    }

    #[test]
    fn sample_repo_reads_branch_and_diffs() {
        let scratch =
            std::env::temp_dir().join(format!("cmux-git-sampler-repo-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&scratch)
                .args(args)
                .output()
                .expect("git available");
            if !out.status.success() {
                panic!(
                    "git {args:?} failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        };
        git(&["init", "-b", "port-main"]);
        let cwd = scratch.clone();
        // Seeded commit: a clean tree reads the branch with no marker.
        std::fs::write(scratch.join("seed.txt"), b"alpha\n").expect("write");
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=cmux",
            "-c",
            "user.email=cmux@local",
            "commit",
            "-m",
            "seed",
        ]);
        let meta = sample_git(&cwd).expect("repo sample");
        assert_eq!(meta.branch, "port-main", "branch line parses");
        assert!(!meta.dirty, "clean tree after commit: {meta:?}");
        assert_eq!(meta.branch_badge(), Some("[port-main]".to_owned()));
        assert_eq!((meta.staged_adds, meta.staged_dels), (0, 0));

        // Stage a 1→4 line edit: the diff chip moves, the marker flips.
        std::fs::write(scratch.join("seed.txt"), b"alpha\nbeta\ngamma\ndelta\n").expect("write");
        git(&["add", "seed.txt"]);
        let meta = sample_git(&cwd).expect("repo sample after edit");
        let _ = std::fs::remove_dir_all(&scratch);
        assert_eq!((meta.staged_adds, meta.staged_dels), (3, 0), "1→4 lines");
        assert!(meta.dirty);
        assert_eq!(meta.branch_badge(), Some("[port-main*]".to_owned()));
    }
}
