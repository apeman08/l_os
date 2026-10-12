//! Builds the ring-3 programs in `user/` for the kernel target and exposes the
//! resulting ELF files to the kernel through `OUT_DIR/user/<name>.elf`.
//!
//! The nested cargo runs from a neutral directory with a scrubbed environment
//! so that neither the kernel's `.cargo/config.toml` nor inherited cargo/rustc
//! variables leak into the user build.

use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let user = manifest.join("user");

    for p in ["user/src", "user/Cargo.toml", "user/Cargo.lock", "user/link.ld", "crates/l_core/src", "crates/l_core/Cargo.toml", "x86_64-l_os.json", "build.rs"] {
        println!("cargo:rerun-if-changed={p}");
    }

    let target_dir = out.join("user-target");
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    for (k, _) in env::vars() {
        let keep_cargo = k == "CARGO_HOME" || k.starts_with("CARGO_HTTP") || k.starts_with("CARGO_NET");
        let scrub = (k.starts_with("CARGO_") && !keep_cargo)
            || k.starts_with("RUSTC")
            || k.starts_with("RUSTDOC")
            || matches!(k.as_str(), "RUSTFLAGS" | "TARGET" | "HOST" | "PROFILE" | "OPT_LEVEL" | "DEBUG" | "NUM_JOBS" | "OUT_DIR");
        if scrub {
            cmd.env_remove(&k);
        }
    }
    let link_script = user.join("link.ld");
    cmd.current_dir(env::temp_dir())
        .env(
            "RUSTFLAGS",
            // Position-independent code because the user window is far above
            // 4 GiB (the target's small code model cannot reach it); the
            // output stays a static, non-relocated ET_EXEC.
            format!("-C link-arg=-T{} -C relocation-model=pic -C link-arg=--no-pie", link_script.display()),
        )
        .args(["build", "--release", "--locked", "--bins", "--manifest-path"])
        .arg(user.join("Cargo.toml"))
        .arg("--target")
        .arg(manifest.join("x86_64-l_os.json"))
        .arg("--target-dir")
        .arg(&target_dir)
        .args([
            "-Zjson-target-spec",
            "-Zbuild-std=core,compiler_builtins",
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
