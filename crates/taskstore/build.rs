// Simple pattern for git describe -> version
use std::process::Command;

fn main() {
    let git_describe = Command::new("git")
        .args(["describe", "--tags", "--match", "taskstore-v*", "--always"])
        .output()
        .and_then(|output| {
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                Err(std::io::Error::other("git describe failed"))
            }
        })
        .unwrap_or_else(|_| {
            // Fallback to Cargo.toml version when git describe fails
            env!("CARGO_PKG_VERSION").to_string()
        });

    let git_describe = match git_describe.strip_prefix("taskstore-v") {
        Some(rest) => rest.to_string(),
        None => git_describe,
    };

    println!("cargo:rustc-env=GIT_DESCRIBE={}", git_describe);
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/");
}
