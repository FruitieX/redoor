use std::process::Command;

fn main() {
    // Bake git/build identity into every binary that links this crate so the server home
    // page and agent details can show exactly which revision operators are running
    // without probing the host filesystem at runtime. It lives in core because both the
    // agent and the server report it.
    emit_build_identity();
}

/// Emits git/build identity env vars consumed via `env!()` at runtime.
fn emit_build_identity() {
    // Rebuild when HEAD moves or the index changes so dirty/clean flips stay accurate.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    if let Ok(head) = std::fs::read_to_string("../../.git/HEAD")
        && let Some(reference) = head.strip_prefix("ref: ").map(str::trim)
    {
        println!("cargo:rerun-if-changed=../../.git/{reference}");
    }

    let git_rev = git_stdout(["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let git_dirty = git_stdout(["status", "--porcelain"])
        .map(|status| !status.is_empty())
        .unwrap_or(false);
    let package_version =
        std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    // Release tags are `v{CARGO_PKG_VERSION}`; HEAD must point at that tag for a clean version.
    let expected_tag = format!("v{package_version}");
    let version_dirty = git_stdout(["tag", "--points-at", "HEAD"])
        .map(|tags| !tags.lines().map(str::trim).any(|tag| tag == expected_tag))
        .unwrap_or(true);
    let build_profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
    // Wall-clock compile time helps operators tell two same-SHA dirty rebuilds apart.
    let build_date = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.is_empty() { None } else { Some(text) }
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=REDOOR_GIT_REV={git_rev}");
    println!(
        "cargo:rustc-env=REDOOR_GIT_DIRTY={}",
        if git_dirty { "1" } else { "0" }
    );
    println!(
        "cargo:rustc-env=REDOOR_VERSION_DIRTY={}",
        if version_dirty { "1" } else { "0" }
    );
    println!("cargo:rustc-env=REDOOR_BUILD_PROFILE={build_profile}");
    println!("cargo:rustc-env=REDOOR_BUILD_DATE={build_date}");
}

/// Runs `git` and returns trimmed stdout when the command succeeds.
fn git_stdout(args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Some(text)
}
