//! 1. Builds the ring-3 programs in `user/` for the kernel target and exposes
//!    the resulting ELF files to the kernel through `OUT_DIR/user/<name>.elf`.
//! 2. Builds the host tools in `tools/image` (`l_os_image`, and `l_os_run`, the
//!    cargo runner named in `.cargo/config.toml`) into `target/l_os-tools`, so
//!    the runner exists before `cargo run` / `cargo test` call it, on every
//!    host and without a shell. The first build compiles the bootloader stages
//!    from crates.io (about a minute, needs network).
//!
//! Both nested cargo builds run from a neutral directory with a scrubbed
//! environment so that neither the kernel's `.cargo/config.toml` (kernel
//! target, build-std) nor inherited cargo/rustc variables leak into them. They
//! use the cargo that runs this build (`CARGO`, the pinned nightly).

use std::{env, fs, path::PathBuf, process::Command};

/// The cargo running this build, in a neutral directory, with the variables
/// that cargo sets for build scripts (or that the user set for the kernel
/// build) removed.
fn nested_cargo() -> Command {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    for (k, _) in env::vars_os() {
        let Some(k) = k.to_str() else { continue };
        let keep_cargo = k == "CARGO_HOME" || k.starts_with("CARGO_HTTP") || k.starts_with("CARGO_NET");
        let scrub = (k.starts_with("CARGO_") && !keep_cargo)
            || k.starts_with("__CARGO")
            || k.starts_with("RUSTC")
            || k.starts_with("RUSTDOC")
            || matches!(k, "RUSTFLAGS" | "TARGET" | "HOST" | "PROFILE" | "OPT_LEVEL" | "DEBUG" | "NUM_JOBS" | "OUT_DIR");
        if scrub {
            cmd.env_remove(k);
        }
    }
    cmd.current_dir(env::temp_dir());
    cmd
}

/// `tools/image` for the host: `target/l_os-tools/debug/{l_os_image,l_os_run}`
/// (`.exe` on Windows). The runner path in `.cargo/config.toml` points there.
fn build_host_tools(manifest: &std::path::Path) {
    let output = nested_cargo()
        .args(["build", "--quiet", "--locked", "--bins", "--manifest-path"])
        .arg(manifest.join("tools").join("image").join("Cargo.toml"))
        .arg("--target-dir")
        .arg(manifest.join("target").join("l_os-tools"))
        .output()
        .expect("failed to launch cargo for tools/image");
    if !output.status.success() {
        panic!(
            "building the host tools (tools/image: image builder and cargo runner) failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let user = manifest.join("user");

    for p in [
        "user/src",
        "user/Cargo.toml",
        "user/Cargo.lock",
        "user/link.ld",
        "crates/l_core/src",
        "crates/l_core/Cargo.toml",
        "x86_64-l_os.json",
        "build.rs",
        "tools/image/src",
        "tools/image/Cargo.toml",
        "tools/image/Cargo.lock",
    ] {
        println!("cargo:rerun-if-changed={p}");
    }

    build_host_tools(&manifest);

    let target_dir = out.join("user-target");
    let mut cmd = nested_cargo();
    let link_script = user.join("link.ld");
    cmd.current_dir(env::temp_dir())
        .env(
            "RUSTFLAGS",
            // Position-independent code because the user window is far above
            // 4 GiB (the target's small code model cannot reach it); the
            // output stays a static, non-relocated ET_EXEC.
            format!(
                "-C link-arg=-T{} -C relocation-model=pic -C link-arg=--no-pie {}",
                link_script.display(),
                // The target has no SSE: force the portable (constant-time)
                // software backends of the RustCrypto crates.
                r#"--cfg curve25519_dalek_backend="serial" --cfg polyval_force_soft --cfg aes_force_soft --cfg chacha20_force_soft --cfg poly1305_force_soft --cfg sha2_backend="soft" --cfg ghash_force_soft"#
            ),
        )
        .args(["build", "--release", "--locked", "--bins", "--manifest-path"])
        .arg(user.join("Cargo.toml"))
        .arg("--target")
        .arg(manifest.join("x86_64-l_os.json"))
        .arg("--target-dir")
        .arg(&target_dir)
        .args([
            "-Zjson-target-spec",
            "-Zbuild-std=core,alloc,compiler_builtins",
            "-Zbuild-std-features=compiler-builtins-mem",
        ]);
    let output = cmd.output().expect("failed to launch cargo for the user programs");
    if !output.status.success() {
        panic!(
            "building user programs failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let dest = out.join("user");
    fs::create_dir_all(&dest).expect("create OUT_DIR/user");
    let built = target_dir.join("x86_64-l_os").join("release");
    for entry in fs::read_dir(user.join("src").join("bin")).expect("read user/src/bin") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path.file_stem().and_then(|s| s.to_str()).expect("bin name").to_owned();
        fs::copy(built.join(&name), dest.join(format!("{name}.elf")))
            .unwrap_or_else(|e| panic!("missing user binary {name}: {e}"));
    }
}
