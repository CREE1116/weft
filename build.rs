use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn watch_tree(path: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        if child
            .file_name()
            .is_some_and(|name| name == ".git" || name == "target" || name == "dist")
        {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            watch_tree(&child);
        } else {
            println!("cargo:rerun-if-changed={}", child.display());
        }
    }
}

fn main() {
    let commit = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain", "--untracked-files=normal"])
        .is_some_and(|status| !status.is_empty());
    let built_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    println!("cargo:rustc-env=WEFT_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=WEFT_GIT_DIRTY={dirty}");
    println!("cargo:rustc-env=WEFT_BUILD_UNIX={built_at}");
    println!(
        "cargo:rustc-env=WEFT_BUILD_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=WEFT_BUILD_TARGET={}",
        std::env::var("TARGET").unwrap_or_default()
    );
    watch_tree(std::path::Path::new("."));
    println!("cargo:rerun-if-changed=.git/HEAD");
}
