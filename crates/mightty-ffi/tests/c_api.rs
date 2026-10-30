//! Builds `tests/c/smoke.c` against `include/mightty.h` and the static
//! library with the system C compiler, then runs it. Also checks the header
//! compiles as C++.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Builds `libmightty.a` the way an embedder would. `cargo test` only builds
/// the rlib, so this runs its own `cargo build` into a separate target dir.
fn static_lib() -> PathBuf {
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("libmightty");
    let status = Command::new(env!("CARGO"))
        .args(["build", "--quiet", "--lib", "--package", "mightty-ffi", "--target-dir"])
        .arg(&target_dir)
        .status()
        .expect("failed to run cargo");
    assert!(status.success(), "building libmightty failed");
    target_dir.join("debug/libmightty.a")
}

fn compiler() -> String {
    env::var("CC").unwrap_or_else(|_| "cc".into())
}

#[test]
fn c_program_against_header() {
    let lib = static_lib();
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("mightty-smoke");

    let mut cc = Command::new(compiler());
    cc.args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-pedantic"])
        .arg("-I")
        .arg(manifest_dir().join("include"))
        .arg(manifest_dir().join("tests/c/smoke.c"))
        .arg(&lib)
        .arg("-o")
        .arg(&out);
    // System libraries the Rust standard library links against.
    if cfg!(target_os = "linux") {
        cc.args(["-lpthread", "-ldl", "-lm"]);
    }
    let status = cc.status().expect("failed to run the C compiler");
    assert!(status.success(), "C compilation failed");

    let run = Command::new(&out).output().unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "C smoke test failed:\n{stderr}");
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
}

#[test]
fn header_is_valid_cpp() {
    let status = Command::new(compiler())
        .args(["-x", "c++", "-std=c++11", "-Wall", "-Wextra", "-Werror", "-pedantic", "-fsyntax-only"])
        .arg(manifest_dir().join("include/mightty.h"))
        .status()
        .expect("failed to run the C++ compiler");
    assert!(status.success(), "header does not compile as C++");
}
