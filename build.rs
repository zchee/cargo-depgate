//! Records the target triple this binary is compiled for.
//!
//! `--platform host` resolves the host through `rustc -vV`, which is the value cargo itself
//! resolves a bare build to. When rustc cannot be reached — a stripped container, a `PATH`
//! without a toolchain — the triple recorded here stands in, and it is right on every machine
//! that runs the binary this build produced. `TARGET` is set by cargo for build scripts only,
//! which is why it has to be captured here rather than read at run time.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo::rustc-env=DEPGATE_HOST_TARGET={target}");
}
