//! Compiles Flecs's amalgamated source (vendor/flecs/distr/flecs.c, upstream master: vendor/flecs/SOURCE.md) and csrc/glue.c with cc, for the host
//! and for wasm32-unknown-unknown. Adapted from den's vendor/hyperion-flecs-c/build.rs.
//!
//! Addons: FLECS_CUSTOM_BUILD plus LOG (error messages), SCRIPT (which pulls in META, DOC and PARSER), PREFAB and
//! CONSTRAINT_TRAITS (OneOf, Final, Symmetric, Acyclic, Trait, Relationship, Target: without it a custom build has no
//! such entities, and a script naming one fails with "unresolved reference").
//! META with PREFAB pulls in QUERY_PLANS (Flecs's addon_defines.h): meta's observers match inherited parts, which
//! need a query plan. No JSON, REST, HTTP, systems or pipeline: nothing here needs threads or a clock.
//!
//! The `rest` feature (native only; refused for wasm32 and host mode) adds REST, which pulls in HTTP, JSON and
//! PIPELINE (so SYSTEM and TIMER), and GLUE_REST for csrc/glue.c: Flecs's REST API over a world, served one request
//! at a time through `glue_rest_request` (no sockets or threads; the caller owns the listener), and the real clock,
//! which the HTTP layer's request cache needs to expire entries. Without it the build is byte for byte what it was.
//!
//! Assert policy (as den's): NDEBUG plus FLECS_KEEP_ASSERT keeps every ecs_check and ecs_assert but drops the debug-only
//! bookkeeping, and FLECS_SOFT_ASSERT makes ecs_check (argument and constraint checks) log and fail the call instead of
//! aborting. Internal sanity checks still abort: on wasm that is a trap, natively a process abort.
//!
//! `-ffp-contract=off`: no fused multiply-add. Script expressions are evaluated in C, and a fused a*b+c rounds once
//! where wasm (which has no FMA) rounds twice, so the same script would give different bits on aarch64 and in wasm.
//!
//! wasm32-unknown-unknown has no C library: `wasm-libc/` is a declaration-only set of headers plus `libc.c` (strings,
//! ctype, printf, stub files). Flecs sets `ECS_TARGET_WASM` and `ECS_TARGET_POSIX` for
//! `__wasm__`, so it uses the plain C calls and no backtraces. malloc, calloc, realloc, free,
//! abort and strtod come from Rust (src/lib.rs). Use an unwrapped clang for wasm (`CC_wasm32_unknown_unknown`,
//! `AR_wasm32_unknown_unknown`): a nix cc-wrapper adds host flags.
//!
//! The library is cached under `<target dir>/flecs-c/<key>/`, the key a SHA-256 of the compiler, the target, every
//! define and flag, and every source and header. Cargo builds this crate once per unit (a build dependency, a normal
//! one, each profile), and every unit that asks for the same library copies it instead of compiling flecs.c again.
//! `FLECS_C_CACHE` names another directory; a target dir with no `CACHEDIR.TAG` above `OUT_DIR` gets no cache.
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    // The repository's one amalgamation (vendor/flecs/distr), in both modes: host mode compiles glue.c against the
    // flecs.h that crates/flecs_ecs_sys compiles, so the glue and the host's Flecs always agree. `FLECS_DISTR` names
    // another directory (flecs_ecs_sys reads the same variable).
    let host = env::var_os("CARGO_FEATURE_HOST").is_some();
    println!("cargo:rerun-if-env-changed=FLECS_DISTR");
    let distr = env::var("FLECS_DISTR").ok();
    let candidates: Vec<&str> = match &distr {
        Some(d) => vec![d.as_str()],
        None => vec!["../../vendor/flecs/distr"],
    };
    let flecs = candidates
        .iter()
        .map(|p| root.join(p))
        .find(|d| d.join("flecs.h").is_file())
        .unwrap_or_else(|| {
            panic!(
                "flecs.h not found under any of {candidates:?} from {} (vendored: vendor/flecs/SOURCE.md)",
                root.display()
            )
        });
    let target = env::var("TARGET").expect("TARGET");
    let rest = env::var_os("CARGO_FEATURE_REST").is_some();
    if rest && (host || target.starts_with("wasm32")) {
        panic!(
            "the rest feature is native only and not for host mode (target {target}, host {host})"
        );
    }
    let mut b = cc::Build::new();
    if host {
        // Host mode: only the glue, against the host's Flecs (flecs_ecs_sys, which this crate depends on, so the
        // linker sees glue before Flecs). The defines that change Flecs's struct layouts must match the host's
        // build: flecs_ecs_sys compiles FLECS_DEBUG unless the profile is release (its build.rs), FLECS_CPP, and
        // its always-on addons (its build.rs `ALWAYS`): CACHED_QUERIES and QUERY_PLANS size `ecs_iter_t`, which the
        // glue keeps on its stack.
        let release = env::var("PROFILE").is_ok_and(|p| p == "release");
        b.file(root.join("csrc/glue.c"))
            .include(&flecs)
            .define("FLECS_CUSTOM_BUILD", None)
            .define("FLECS_CPP", None)
            .define("FLECS_META", None)
            .define("FLECS_SCRIPT", None)
            .define("FLECS_LOG", None)
            .define("FLECS_CACHED_QUERIES", None)
            .define("FLECS_QUERY_PLANS", None)
            .define("FLECS_QUERY_DSL", None)
            .define("FLECS_PARSER", None)
            .define("FLECS_PREFAB", None)
            .define("FLECS_CONSTRAINT_TRAITS", None)
            .define("FLECS_ENTITY_RANGES", None)
            .define("FLECS_MULTI_WORLD", None)
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
    let wasm = target.starts_with("wasm32");
    let mut defines = vec![
        "FLECS_CUSTOM_BUILD",
        "FLECS_LOG",
        "FLECS_SCRIPT",
        "FLECS_PREFAB",
        "FLECS_CONSTRAINT_TRAITS",
        "NDEBUG",
        "FLECS_KEEP_ASSERT",
        "FLECS_SOFT_ASSERT",
    ];
    if rest {
        defines.extend(["FLECS_REST", "GLUE_REST"]);
    }
    let mut flags = vec!["-std=gnu99".to_string(), "-ffp-contract=off".to_string()];
    let mut optional = vec![format!(
        "-ffile-prefix-map={}=.",
        root.join("../..").display()
    )];
    let mut files = vec![flecs.join("flecs.c"), root.join("csrc/glue.c")];
    let mut includes = vec![flecs.clone()];
    if wasm {
        let libc = root.join("wasm-libc");
        flags.extend(["-ffreestanding".to_string(), "-nostdlibinc".to_string()]);
        includes.push(libc.join("include"));
        files.push(libc.join("libc.c"));
    } else {
        optional.push("-fno-sanitize=undefined".to_string());
    }
    for f in &files {
        b.file(f);
    }
    for i in &includes {
        b.include(i);
    }
    for d in &defines {
        b.define(d, None);
    }
    for f in &flags {
        b.flag(f);
    }
    for f in &optional {
        b.flag_if_supported(f);
    }
    b.opt_level(2).debug(false).warnings(false);
    for p in [
        flecs.join("flecs.c"),
        flecs.join("flecs.h"),
        root.join("csrc"),
        root.join("wasm-libc"),
    ] {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    let mut sources = vec![
        flecs.join("flecs.c"),
        flecs.join("flecs.h"),
        root.join("csrc"),
    ];
    if wasm {
        sources.push(root.join("wasm-libc"));
    }
    println!("cargo:rerun-if-env-changed=CC_wasm32_unknown_unknown");
    println!("cargo:rerun-if-env-changed=AR_wasm32_unknown_unknown");
    println!("cargo:rerun-if-env-changed=FLECS_C_CACHE");

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let lib = if target.contains("msvc") {
        "flecs.lib"
    } else {
        "libflecs.a"
    };
    let Some(cache) = cache_dir(&out) else {
        b.compile("flecs");
        return;
    };
    let recipe =
        format!("{target}\n{defines:?}\n{flags:?}\n{optional:?}\nopt 2, no debug, no warnings");
    let key = match key(&b, &recipe, &sources) {
        Ok(k) => k,
        Err(e) => panic!("flecs-c cache key: {e}"),
    };
    let entry = cache.join(&key);
    if let Err(e) = fs::create_dir_all(&cache) {
        panic!("flecs-c cache {}: {e}", cache.display());
    }
    let lock_path = cache.join(format!("{key}.lock"));
    let lock = fs::File::create(&lock_path)
        .unwrap_or_else(|e| panic!("flecs-c cache lock {}: {e}", lock_path.display()));
    // Units that build at once wait here for one compile instead of each running their own.
    lock.lock()
        .unwrap_or_else(|e| panic!("flecs-c cache lock {}: {e}", lock_path.display()));
    if entry.join(lib).is_file() {
        if let Err(e) = fs::copy(entry.join(lib), out.join(lib)) {
            panic!("flecs-c cache {}: {e}", entry.display());
        }
        println!("cargo:rustc-link-search=native={}", out.display());
        println!("cargo:rustc-link-lib=static=flecs");
        return;
    }
    b.compile("flecs");
    if let Err(e) = publish(&out.join(lib), &entry, lib) {
        panic!("flecs-c cache {}: {e}", entry.display());
    }
}

/// `FLECS_C_CACHE`, else `flecs-c/` in the target dir: the nearest directory above `out` holding Cargo's
/// `CACHEDIR.TAG`.
fn cache_dir(out: &Path) -> Option<PathBuf> {
    if let Some(dir) = env::var_os("FLECS_C_CACHE") {
        return Some(PathBuf::from(dir));
    }
    out.ancestors()
        .find(|d| d.join("CACHEDIR.TAG").is_file())
        .map(|d| d.join("flecs-c"))
}

/// The SHA-256 of the compiler (its path, arguments and `--version`), `recipe`, and every file under `sources`.
fn key(b: &cc::Build, recipe: &str, sources: &[PathBuf]) -> io::Result<String> {
    let cc = b.try_get_compiler().map_err(io::Error::other)?;
    let version = Command::new(cc.path()).arg("--version").output()?;
    let mut h = Sha256::new();
    h.update(format!("{:?}\n{:?}\n", cc.path(), cc.args()));
    h.update(&version.stdout);
    h.update(recipe);
    let mut files = vec![];
    for p in sources {
        walk(p, &mut files)?;
    }
    files.sort();
    for f in files {
        let bytes = fs::read(&f)?;
        h.update(format!("\n{}\n{}\n", f.display(), bytes.len()));
        h.update(bytes);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn walk(p: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    if p.is_dir() {
        for e in fs::read_dir(p)? {
            walk(&e?.path(), files)?;
        }
    } else {
        files.push(p.to_path_buf());
    }
    Ok(())
}

/// Puts `lib` in the cache as `entry/<name>`, through a rename so a reader never sees half a library.
fn publish(lib: &Path, entry: &Path, name: &str) -> io::Result<()> {
    let tmp = entry.with_extension(format!("tmp{}", std::process::id()));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(&tmp)?;
    fs::copy(lib, tmp.join(name))?;
    if entry.exists() {
        fs::remove_dir_all(entry)?;
    }
    fs::rename(&tmp, entry)
}
