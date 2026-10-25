"""Builds everything: the wasm module and the native libraries (cargo), then the TS package (tsc) with the wasm in
dist/. With --test, also runs the Rust tests and the bun tests.

    uv run build.py [--test]

Needs cargo (the toolchain in rust-toolchain.toml), bun, and an unwrapped clang and llvm-ar for wasm32
(CC_wasm32_unknown_unknown, AR_wasm32_unknown_unknown).
"""

import argparse
import asyncio
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
    cargo = ["cargo", "build", "--release", "-p", "flecs-snapshot"]
    async with asyncio.TaskGroup() as g:
        g.create_task(run("cargo wasm", [*cargo, "--target", WASM_TARGET], 900))
        g.create_task(run("cargo native", cargo, 900))
        g.create_task(run("bun install", ["bun", "install", "--frozen-lockfile"], 300))
    dist = ROOT / "dist"
    shutil.rmtree(dist, ignore_errors=True)
    await run("tsc", ["bun", "x", "tsc", "-p", "tsconfig.json"], 300)
    wasm = target_dir() / WASM_TARGET / "release" / "flecs_snapshot.wasm"
    shutil.copyfile(wasm, dist / "flecs.wasm")
    print(f"dist/flecs.wasm: {wasm.stat().st_size} bytes", flush=True)
    if not test:
        return
    ext = "dylib" if sys.platform == "darwin" else "so"
    os.environ.setdefault("FLECS_NATIVE", str(target_dir() / "release" / f"libflecs_snapshot.{ext}"))
    async with asyncio.TaskGroup() as g:
        g.create_task(run("cargo test", ["cargo", "test", "--workspace"], 900))
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
