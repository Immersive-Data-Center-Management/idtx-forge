use std::{path::PathBuf, process::Command};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let git = |args: &[&str]| Command::new("git").current_dir(&root).args(args).output().ok().filter(|o| o.status.success()).map(|o| o.stdout).unwrap_or_default();
    let head = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"])).trim().to_owned();
    let branch = String::from_utf8_lossy(&git(&["symbolic-ref", "HEAD"])).trim().to_owned();
    for reference in ["HEAD", branch.as_str()] {
        if reference.is_empty() { continue; }
        let path = String::from_utf8_lossy(&git(&["rev-parse", "--git-path", reference])).trim().to_owned();
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    // A renderer/session edit can rebuild the runtime without changing this
    // crate. Refresh the dirty-source identity for all runtime source inputs.
    for input in ["crates", "apps", "templates", "Cargo.toml", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", root.join(input).display());
    }
    // Tracked working-tree changes AND untracked (non-ignored) files both make
    // the runtime diverge from HEAD. `git diff HEAD` misses untracked files, so
    // a runtime compiled from a brand-new, not-yet-added source or template file
    // would otherwise be stamped as a clean HEAD build. `git ls-files --others
    // --exclude-standard` lists exactly those untracked-but-not-ignored paths
    // (so `target/` and other ignored noise never mark the build dirty).
    let diff = git(&["diff", "HEAD", "--", "."]);
    let untracked = git(&["ls-files", "--others", "--exclude-standard", "--", "."]);
    let dirty = [diff.as_slice(), untracked.as_slice()].concat();
    let suffix = if dirty.is_empty() { String::new() } else {
        let hash = Sha256::digest(&dirty).iter().fold(String::new(), |mut text, byte| {
            let _ = write!(text, "{byte:02x}"); text
        });
        format!("-dirty-{hash}")
    };
    println!("cargo:rustc-env=IDTX_CAPTURE_REVISION={head}{suffix}");
}
