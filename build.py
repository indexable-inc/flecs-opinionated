"""Builds everything: the wasm module and the native libraries (cargo), then the TS package (tsc) with the wasm in
dist/. With --test, also runs the Rust tests and the bun tests.

    uv run build.py [--test]

Needs cargo (the toolchain in rust-toolchain.toml), bun, an unwrapped clang and llvm-ar for wasm32
(CC_wasm32_unknown_unknown, AR_wasm32_unknown_unknown), and binaryen's wasm-opt (WASM_OPT, or on PATH; CI pins
version_133).
"""

import argparse
import asyncio
import gzip
import os
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent
WASM_TARGET = "wasm32-unknown-unknown"


@dataclass
class StepError(Exception):
    step: str
    command: list[str]
    code: int | None
    stderr: str

    def __str__(self) -> str:
        tail = "\n".join(self.stderr.strip().splitlines()[-30:])
        status = "timed out" if self.code is None else f"exit {self.code}"
        return f"step {self.step!r} failed ({status}): {' '.join(self.command)}\n{tail}"


async def run(step: str, command: list[str], limit: float) -> None:
    print(f"[{step}] {' '.join(command)}", flush=True)
    proc = await asyncio.create_subprocess_exec(
        *command, cwd=ROOT, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT
    )
    try:
        async with asyncio.timeout(limit):
            out, _ = await proc.communicate()
    except TimeoutError:
        proc.kill()
        out, _ = await proc.communicate()
        raise StepError(step, command, None, out.decode(errors="replace")) from None
    text = out.decode(errors="replace")
    if proc.returncode != 0:
        raise StepError(step, command, proc.returncode, text)
    print(f"[{step}] done", flush=True)


def target_dir() -> Path:
    return Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))


async def build(test: bool) -> None:
    wasm_opt = os.environ.get("WASM_OPT") or shutil.which("wasm-opt")
    if not wasm_opt:
        raise StepError("find wasm-opt", ["wasm-opt"], 127, "set WASM_OPT or put binaryen's wasm-opt on PATH")
    # The C ABI exports are behind the ffi feature (off for crates that depend on flecs-snapshot), and the library
    # kinds are chosen here: the crate itself is an rlib, so a host that links it gets no second cdylib.
    cargo = ["cargo", "rustc", "--release", "-p", "flecs-snapshot", "--features", "ffi"]
    async with asyncio.TaskGroup() as g:
        g.create_task(run("cargo wasm", [*cargo, "--target", WASM_TARGET, "--crate-type", "cdylib"], 900))
        g.create_task(run("cargo native", [*cargo, "--crate-type", "cdylib,staticlib"], 900))
        g.create_task(run("bun install", ["bun", "install", "--frozen-lockfile"], 300))
    dist = ROOT / "dist"
    shutil.rmtree(dist, ignore_errors=True)
    await run("tsc", ["bun", "x", "tsc", "-p", "tsconfig.json"], 300)
    wasm = target_dir() / WASM_TARGET / "release" / "flecs_snapshot.wasm"
    out = dist / "flecs.wasm"
    # The features rustc's wasm32-unknown-unknown target enables by default; wasm-opt must keep to the same set.
    features = [
        "--enable-bulk-memory",
        "--enable-sign-ext",
        "--enable-mutable-globals",
        "--enable-nontrapping-float-to-int",
        "--enable-multivalue",
        "--enable-reference-types",
    ]
    opt = [wasm_opt, "-Os", *features, "--strip-debug", "--strip-producers", str(wasm), "-o", str(out)]
    await run("wasm-opt", opt, 600)
    raw = wasm.stat().st_size
    size = out.stat().st_size
    gz = len(gzip.compress(out.read_bytes(), 9))
    print(f"dist/flecs.wasm: {size} bytes ({gz} gzipped), {raw} before wasm-opt", flush=True)
    if not test:
        return
    ext = "dylib" if sys.platform == "darwin" else "so"
    os.environ.setdefault("FLECS_NATIVE", str(target_dir() / "release" / f"libflecs_snapshot.{ext}"))
    async with asyncio.TaskGroup() as g:
        # Not --all-features: flecs-c's `host` links the application's Flecs, which host-check/ supplies.
        features = "flecs-snapshot/ffi,flecs-describe/glam,flecs-describe/glamx,flecs-describe/half,flecs-describe/bitflags"
        g.create_task(run("cargo test", ["cargo", "test", "--workspace", "--features", features], 900))
        # The native-only REST server, on its own: in the workspace run it would change the native worlds the
        # byte-identity tests compare with wasm (REST imports the pipeline, timer and REST modules into every world).
        g.create_task(run("cargo test rest", ["cargo", "test", "-p", "flecs-c", "--features", "rest"], 900))
        # Host mode against flecs_ecs at the space game's pin (its own workspace, patched to vendor/flecs_ecs_sys).
        g.create_task(run("host-check", ["cargo", "test", "--manifest-path", "host-check/Cargo.toml"], 900))
        g.create_task(run("bun test", ["bun", "test"], 300))
        g.create_task(run("tsc tests", ["bun", "x", "tsc", "-p", "tsconfig.test.json"], 300))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--test", action="store_true", help="also run the Rust and bun tests")
    args = parser.parse_args()
    try:
        asyncio.run(build(args.test))
    except* StepError as group:
        for e in group.exceptions:
            print(e, file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
