// Phase 0 build script: compile the msys POSIX shim (C) into a static lib
// that wash links against. The shim uses the real msys/cygwin <fcntl.h>
// constants so we never hardcode O_CREAT/O_TRUNC/etc.
//
// The shim MUST be compiled by the *msys* gcc (the one that targets
// x86_64-pc-cygwin and links msys-2.0.dll), NOT the MinGW-w64 gcc that may
// sit earlier on PATH. We derive the correct gcc from `rustc --print sysroot`
// so the object is ABI-compatible with the binary Rust produces.

use std::path::PathBuf;

fn main() {
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
