// Phase 0 build script: compile the msys POSIX shim (C) into a static lib
// that wash links against. The shim uses the real msys/cygwin <fcntl.h>
// constants so we never hardcode O_CREAT/O_TRUNC/etc.
//
// The shim MUST be compiled by the *msys* gcc (the one that targets
// x86_64-pc-cygwin and links msys-2.0.dll), NOT the MinGW-w64 gcc that may
// sit earlier on PATH. We derive the correct gcc from `rustc --print sysroot`
// so the object is ABI-compatible with the binary Rust produces.

use std::path::PathBuf;

/// Generate the single version string from Cargo.toml + git tag/HEAD and hand
/// it to the crate as `WASH_VERSION`.  The rule itself lives in
/// `tools/wash_version.sh`; build.rs deliberately has NO second copy of it, so
/// the binary and the test gates can never disagree about what "the version"
/// means.  A failed git lookup is a hard build error, never a fallback.
fn emit_version() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let script = PathBuf::from(&manifest).join("tools/wash_version.sh");
    let out = std::process::Command::new("bash")
        .arg(&script)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", script.display()));
    if !out.status.success() {
        panic!(
            "wash version generation failed ({:?}):\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let version = String::from_utf8(out.stdout)
        .expect("wash_version.sh produced non-UTF-8 output")
        .trim()
        .to_string();
    let base = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    if version != base && !version.starts_with(&format!("{base}-")) {
        panic!("generated version [{version}] does not start with Cargo.toml version [{base}]");
    }
    if version.is_empty() || version.contains(char::is_whitespace) {
        panic!("generated version is empty or contains whitespace: [{version}]");
    }
    println!("cargo:rustc-env=WASH_VERSION={version}");
    println!("cargo:rerun-if-changed=tools/wash_version.sh");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=WASH_GIT_DIR");

    // Re-run when HEAD, a branch ref, a new tag or packed-refs change -- i.e.
    // whenever the generated string could change.  In WSL the git metadata is
    // external (WASH_GIT_DIR), so watch that tree's refs instead.
    if let Ok(g) = std::env::var("WASH_GIT_DIR") {
        let g = g.trim_end_matches('/');
        for p in ["HEAD", "refs", "packed-refs"] {
            println!("cargo:rerun-if-changed={g}/{p}");
        }
    } else if PathBuf::from(&manifest).join(".git").exists() {
        for p in [".git/HEAD", ".git/refs", ".git/packed-refs"] {
            println!("cargo:rerun-if-changed={p}");
        }
    }
}

fn main() {
    emit_version();
    let mut build = cc::Build::new();

    if let Ok(out) = std::process::Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
    {
        if out.status.success() {
            let sysroot = String::from_utf8_lossy(&out.stdout);
            let sysroot = sysroot.trim();
            let candidate: PathBuf = PathBuf::from(sysroot).join("bin/gcc.exe");
            if candidate.exists() {
                build.compiler(candidate);
            }
        }
    }

    build.file("src/posix_shim.c").compile("washposix");
    // 故意不 emit `cargo:rustc-link-lib=dl`：P0 spike（docs/spike-dlopen.md）
    // 已证实 MSYS2(Cygwin) 与 WSL/glibc 的默认链接都直接提供
    // dlopen/dlsym/dlclose/dlerror，加 -ldl 反而可能引入不存在的 libdl 依赖。
    // 若将来某平台最终链接报 "undefined reference to dlopen"，再按
    // CARGO_CFG_TARGET_OS 分支加回（不要用 cfg!(unix)，该 target 下为假）。
    println!("cargo:rerun-if-changed=src/posix_shim.c");
    println!("cargo:rerun-if-changed=build.rs");
}
