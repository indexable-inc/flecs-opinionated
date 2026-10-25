"""Regenerate the Lean translation of `merge/` and check the proofs.

    uv run lean/regen.py           # translate, build, write vectors, cargo test
    uv run lean/regen.py --check   # the same, but fail if a generated file differs from the committed one

Steps: download the pinned Aeneas release (it bundles Charon) and install Charon's Rust toolchain, translate
`merge/` with Charon then Aeneas into `lean/Merge/{Types,Funs}.lean`, fetch the Mathlib cache (in parallel with the
translation), `lake build` with no `sorry` allowed, write `merge/tests/vectors.json` with `lake exe vectors`, and
replay the vectors with `cargo test -p merge`. Needs `lake` (elan), `rustup`, `cargo` and `tar` on PATH; Linux
x86_64 or aarch64, or macOS aarch64.
"""

import argparse
import asyncio
import hashlib
import os
import platform
import re
import shutil
import sys
import tempfile
import tomllib
import urllib.request
from pathlib import Path

# Keep AENEAS_REV equal to the aeneas `rev` in lean/lakefile.toml.
AENEAS_TAG = "nightly-2026.10.24-74aabd5"
AENEAS_REV = "74aabd52f77001cf9eaad51b4f4b867a7dc6b111"
CHARON_VERSION = "0.1.294"
SHA256 = {
    "aeneas-linux-x86_64.tar.gz": "0c2d6acf44e3e4f38a9b04ca14f909cf049bed1033a85b4591da5fd46663eeeb",
}

ROOT = Path(__file__).resolve().parent.parent
LEAN = ROOT / "lean"
CRATE = ROOT / "merge"
TOOLS = LEAN / ".tools" / AENEAS_TAG
GENERATED = ["Types.lean", "Funs.lean"]
VECTORS = CRATE / "tests" / "vectors.json"


class StepError(Exception):
    pass


def tail(text: str, lines: int = 40) -> str:
    return "\n".join(text.rstrip().splitlines()[-lines:])


async def run(step: str, *cmd: str, cwd: Path, bound: float, env: dict[str, str] | None = None) -> str:
    """Run a command; return its stdout and stderr together, or raise naming the step, command, code and tail."""
    shown = " ".join(cmd)
    try:
        async with asyncio.timeout(bound):
            proc = await asyncio.create_subprocess_exec(
                *cmd,
                cwd=cwd,
                env=env,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.STDOUT,
            )
            out, _ = await proc.communicate()
    except TimeoutError:
        raise StepError(f"{step}: `{shown}` took longer than {bound:.0f} s") from None
    except FileNotFoundError:
        raise StepError(f"{step}: `{cmd[0]}` is not on PATH") from None
    text = out.decode(errors="replace")
    if proc.returncode != 0:
        raise StepError(f"{step}: `{shown}` exited {proc.returncode}\n{tail(text)}")
    return text


def asset() -> str:
    system = {"Linux": "linux", "Darwin": "macos"}.get(platform.system())
    machine = {"x86_64": "x86_64", "AMD64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}.get(platform.machine())
    if system is None or machine is None:
        raise StepError(f"tools: no Aeneas release for {platform.system()} {platform.machine()}")
    return f"aeneas-{system}-{machine}.tar.gz"


def download(url: str, dest: Path, sha: str | None) -> None:
    digest = hashlib.sha256()
    with urllib.request.urlopen(url, timeout=60) as resp, dest.open("wb") as f:
        while chunk := resp.read(1 << 20):
            digest.update(chunk)
            f.write(chunk)
    if sha is not None and digest.hexdigest() != sha:
        dest.unlink()
        raise StepError(f"tools: {url} has sha256 {digest.hexdigest()}, expected {sha}")


async def tools() -> Path:
    """The pinned Aeneas release, unpacked, with Charon's Rust toolchain installed."""
    name = asset()
    if not (TOOLS / "aeneas").exists():
        TOOLS.mkdir(parents=True, exist_ok=True)
        tarball = TOOLS / name
        url = f"https://github.com/AeneasVerif/aeneas/releases/download/{AENEAS_TAG}/{name}"
        try:
            async with asyncio.timeout(900):
                await asyncio.to_thread(download, url, tarball, SHA256.get(name))
        except TimeoutError:
            raise StepError(f"tools: downloading {url} took longer than 900 s") from None
        await run("tools", "tar", "xzf", str(tarball), "-C", str(TOOLS), cwd=TOOLS, bound=300)
        tarball.unlink()
    version = await run("tools", str(TOOLS / "charon"), "version", cwd=TOOLS, bound=60)
    if CHARON_VERSION not in version:
        raise StepError(f"tools: charon reports {version.strip()!r}, expected {CHARON_VERSION}")
    toolchain = tomllib.loads((TOOLS / "rust-toolchain").read_text())["toolchain"]
    components = ",".join(toolchain.get("components", []))
    await run(
        "tools",
        "rustup",
        "toolchain",
        "install",
        toolchain["channel"],
        "--profile",
        "minimal",
        "--component",
        components,
        cwd=TOOLS,
        bound=900,
    )
    return TOOLS


async def translate(work: Path) -> Path:
    """Charon, then Aeneas: the Lean translation of `merge/` in a scratch directory."""
    bin_dir = await tools()
    llbc = work / "merge.llbc"
    out = work / "lean"
    await run(
        "charon",
        str(bin_dir / "charon"),
        "cargo",
        "--preset=aeneas",
        "--dest-file",
        str(llbc),
        cwd=CRATE,
        bound=1200,
        env={**os.environ, "CARGO_TARGET_DIR": str(work / "target")},
    )
    log = await run(
        "aeneas",
        str(bin_dir / "aeneas"),
        "-backend",
        "lean",
        "-split-files",
        "-dest",
        str(out),
        str(llbc),
        cwd=work,
        bound=600,
    )
    if re.search(r"(?i)\berror\b", log):
        raise StepError(f"aeneas: `aeneas -backend lean ...` reported errors\n{tail(log)}")
    return out


def sync(step: str, new: Path, old: Path, check: bool) -> None:
    """Install a generated file, or with --check, fail if it differs from the committed one."""
    if old.exists() and old.read_bytes() == new.read_bytes():
        return
    if check:
        raise StepError(f"{step}: {old.relative_to(ROOT)} is out of date; run `uv run lean/regen.py` and commit it")
    shutil.copyfile(new, old)
    print(f"{step}: wrote {old.relative_to(ROOT)}")


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="fail if a generated file differs from the commit")
    args = parser.parse_args()

    pins = tomllib.loads((LEAN / "lakefile.toml").read_text())
    revs = [r.get("rev") for r in pins.get("require", []) if r.get("name") == "aeneas"]
    if revs != [AENEAS_REV]:
        raise StepError(f"pins: lakefile.toml pins aeneas at {revs}, regen.py at {AENEAS_REV}")

    with tempfile.TemporaryDirectory(prefix="regen-") as tmp:
        work = Path(tmp)
        async with asyncio.TaskGroup() as tg:
            lean_out = tg.create_task(translate(work))
            tg.create_task(run("cache", "lake", "exe", "cache", "get", cwd=LEAN, bound=1800))
        for name in GENERATED:
            sync("aeneas", lean_out.result() / name, LEAN / "Merge" / name, args.check)

        log = await run("lake build", "lake", "build", cwd=LEAN, bound=5400)
        sorries = [line for line in log.splitlines() if "declaration uses `sorry`" in line]
        if sorries:
            raise StepError("lake build: proofs use sorry\n" + "\n".join(sorries))
        print("lake build: ok, no sorry")

        vectors = work / "vectors.json"
        await run("vectors", "lake", "exe", "vectors", str(vectors), cwd=LEAN, bound=600)
        sync("vectors", vectors, VECTORS, args.check)

    await run("cargo test", "cargo", "test", "-p", "merge", cwd=ROOT, bound=1200)
    print("cargo test -p merge: ok")


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except* StepError as group:
        for err in group.exceptions:
            print(err, file=sys.stderr)
        sys.exit(1)
