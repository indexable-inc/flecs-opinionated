//! Compiles Flecs's amalgamated source (vendor/flecs/distr/flecs.c, the space game's pin: vendor/flecs/SOURCE.md) and csrc/glue.c with cc, for the host
//! and for wasm32-unknown-unknown. Adapted from den's vendor/hyperion-flecs-c/build.rs.
//!
//! Addons: FLECS_CUSTOM_BUILD plus LOG (error messages), SCRIPT (which pulls in META, DOC and PARSER) and PREFAB. No
//! JSON, REST, HTTP, systems or pipeline: nothing here needs threads or a clock.
//!
//! Assert policy (as den's): NDEBUG plus FLECS_KEEP_ASSERT keeps every ecs_check and ecs_assert but drops the debug-only
//! bookkeeping, and FLECS_SOFT_ASSERT makes ecs_check (argument and constraint checks) log and fail the call instead of
//! aborting. Internal sanity checks still abort: on wasm that is a trap, natively a process abort.
//!
//! `-ffp-contract=off`: no fused multiply-add. Script expressions are evaluated in C, and a fused a*b+c rounds once
//! where wasm (which has no FMA) rounds twice, so the same script would give different bits on aarch64 and in wasm.
//!
//! wasm32-unknown-unknown has no C library: `wasm-libc/` is a declaration-only set of headers plus `libc.c` (strings,
//! ctype, printf, stub files). `ECS_TARGET_POSIX` makes flecs use the plain C calls. malloc, calloc, realloc, free,
//! abort and strtod come from Rust (src/lib.rs). Use an unwrapped clang for wasm (`CC_wasm32_unknown_unknown`,
//! `AR_wasm32_unknown_unknown`): a nix cc-wrapper adds host flags.
use std::env;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    // This repo's pin (vendor/flecs/distr), or, when the crate is vendored into a host's tree next to its
    // flecs_ecs_sys (`<vendor>/flecs-opinionated/crates/flecs-c` beside `<vendor>/flecs_ecs_sys`), the host's own
    // amalgamation, so a vendored copy needs no edit here.
    let flecs = ["../../vendor/flecs/distr", "../../../flecs_ecs_sys/src"]
        .iter()
        .map(|p| root.join(p))
        .find(|d| d.join("flecs.h").is_file())
        .unwrap_or_else(|| {
            panic!(
                "flecs.h not found under {} or {} (vendored: vendor/flecs/SOURCE.md)",
                root.join("../../vendor/flecs/distr").display(),
                root.join("../../../flecs_ecs_sys/src").display()
            )
        });
    let target = env::var("TARGET").expect("TARGET");
    let mut b = cc::Build::new();
    if env::var_os("CARGO_FEATURE_HOST").is_some() {
        // Host mode: only the glue, against the host's Flecs (flecs_ecs_sys, which this crate depends on, so the
        // linker sees glue before Flecs). The defines that change Flecs's struct layouts must match the host's
        // build: flecs_ecs_sys compiles FLECS_DEBUG unless the profile is release (its build.rs), and FLECS_CPP.
        let release = env::var("PROFILE").is_ok_and(|p| p == "release");
        b.file(root.join("csrc/glue.c"))
            .include(&flecs)
            .define("FLECS_CUSTOM_BUILD", None)
            .define("FLECS_CPP", None)
            .define("FLECS_META", None)
            .define("FLECS_SCRIPT", None)
            .define("FLECS_LOG", None)
            .define("GLUE_HOST", None)
            .opt_level(2)
            .debug(false)
            .warnings(false)
            .flag("-std=gnu99");
        if release {
            b.define("NDEBUG", None).define("FLECS_NDEBUG", None);
        } else {
            b.define("FLECS_DEBUG", None);
        }
        b.compile("flecs_glue");
        println!("cargo:rerun-if-changed={}", root.join("csrc").display());
        println!("cargo:rerun-if-changed={}", flecs.join("flecs.h").display());
        return;
    }
    b.file(flecs.join("flecs.c"))
        .file(root.join("csrc/glue.c"))
        .include(&flecs)
        .define("FLECS_CUSTOM_BUILD", None)
        .define("FLECS_LOG", None)
        .define("FLECS_SCRIPT", None)
        .define("FLECS_PREFAB", None)
        .define("NDEBUG", None)
        .define("FLECS_KEEP_ASSERT", None)
        .define("FLECS_SOFT_ASSERT", None)
        .opt_level(2)
        .debug(false)
        .warnings(false)
        .flag("-std=gnu99")
        .flag("-ffp-contract=off")
        .flag_if_supported(format!(
            "-ffile-prefix-map={}=.",
            root.join("../..").display()
        ));
    if target.starts_with("wasm32") {
        let libc = root.join("wasm-libc");
        b.define("ECS_TARGET_POSIX", None)
            .flag("-ffreestanding")
            .flag("-nostdlibinc")
            .include(libc.join("include"))
            .file(libc.join("libc.c"));
    } else {
        b.flag_if_supported("-fno-sanitize=undefined");
    }
    b.compile("flecs");
    for p in [
        flecs.join("flecs.c"),
        flecs.join("flecs.h"),
        root.join("csrc"),
        root.join("wasm-libc"),
    ] {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    println!("cargo:rerun-if-env-changed=CC_wasm32_unknown_unknown");
    println!("cargo:rerun-if-env-changed=AR_wasm32_unknown_unknown");
}
