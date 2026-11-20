#!/usr/bin/env python3
"""
build_openusd.py -- Clone and build a monolithic OpenUSD library for the
IDTX Studio C++ shim (`crates/idtx-usd-sys`).

This is the workspace-level helper specified by
`docs/decisions/0003-openusd-cxx-boundary.md` and used when no compatible
install prefix exists.

# Behavior

The version pin, default source directory, and default install prefix
live in `openusd_config.toml` next to this script (authoritative per
ADR-0003). All three can be overridden on the command line.

Given the resolved source and install directories, the script:

  1. Verifies the local toolchain (Python >= 3.8, git, cmake, C++
     compiler).
  2. Reuses an existing install if the monolithic library (`usd_ms`)
     is already present under `<install-dir>/lib`, the headers exist
     under `<install-dir>/include`, and (if the version marker file is
     present) the recorded version matches the requested version.
     `--force` bypasses the reuse fast path.
  3. Otherwise clones Pixar's OpenUSD at the pinned tag into the source
     directory (unless one already exists there) and invokes the
     upstream `build_scripts/build_usd.py` with the exact flag set the
     IDTX C++ shim needs.
  4. On Windows, primes an MSVC environment via `vswhere` +
     `vcvars64.bat` and injects it into the build subprocess so the
     script works from a plain PowerShell / cmd session.
  5. After a successful build, writes `<install-dir>/.idtx_openusd_version`
     so subsequent invocations can validate the install matches the
     requested version without a full rebuild.

# Downstream consumer

`crates/idtx-usd-sys/build.rs` reads the install dir from
`$IDTX_USD_DIR` (falling back to
`<workspace>/thirdparty/openusd-26.08-mtlx/install`) and links against
`usd_ms`. Point `IDTX_USD_DIR` at whatever
`--install-dir` you passed here to keep the two in sync across
workspace relocations.

# Usage

    # Default paths from openusd_config.toml
    python tools/build-scripts/build_openusd.py

    # Debug variant
    python tools/build-scripts/build_openusd.py --variant debug

    # Reuse an existing OpenUSD build tree (no clone, no rebuild -- as
    # long as usd_ms exists at <install-dir>/lib and the version marker
    # matches):
    python tools/build-scripts/build_openusd.py \\
        --src-dir     D:/openusd/src \\
        --install-dir D:/openusd/install

    # Force a clean rebuild even if the lib already exists:
    python tools/build-scripts/build_openusd.py --force

    # Just verify prerequisites and print the plan:
    python tools/build-scripts/build_openusd.py --check-only

Requires:
    * Python 3.8+   (stdlib `tomllib` on 3.11+, or `tomli` on 3.8-3.10)
    * git
    * cmake >= 3.26
    * A C++17 toolchain on PATH (MSVC on Windows, clang/gcc elsewhere)

The clone + build is intentionally slow (30-90 minutes on a modern
laptop) and is a one-time setup step.
"""

from __future__ import annotations

import argparse
import os
import platform
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Optional

# tomllib is stdlib in 3.11+; fall back to tomli on older versions.
try:
    import tomllib  # type: ignore[import-not-found]
except ModuleNotFoundError:  # pragma: no cover
    try:
        import tomli as tomllib  # type: ignore[import-not-found, no-redef]
    except ModuleNotFoundError:
        sys.exit(
            "ERROR: this script needs either Python 3.11+ (for stdlib "
            "`tomllib`) or the `tomli` package on Python 3.8-3.10.\n"
            "       pip install tomli"
        )


SCRIPT_DIR = Path(__file__).resolve().parent
WORKSPACE_ROOT = SCRIPT_DIR.parent.parent
CONFIG_PATH = SCRIPT_DIR / "openusd_config.toml"

# Marker file dropped in the install prefix after a successful build so
# later invocations can tell "is this install compatible with the
# requested version?" without re-running the whole build.
VERSION_MARKER = ".idtx_openusd_version"


# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

def load_config() -> dict:
    if not CONFIG_PATH.is_file():
        sys.exit(
            f"ERROR: missing config file {CONFIG_PATH}.\n"
            f"       This file is required (see the [openusd] section)."
        )
    with open(CONFIG_PATH, "rb") as f:
        return tomllib.load(f)["openusd"]


# ---------------------------------------------------------------------------
# Platform helpers
# ---------------------------------------------------------------------------

def is_windows() -> bool:
    return platform.system() == "Windows"


def is_macos() -> bool:
    return platform.system() == "Darwin"


def monolithic_lib_candidates(install_dir: Path) -> list:
    """
    Filenames the upstream `build_usd.py --build-monolithic` step is
    known to produce for `usd_ms` on the current platform.  Any one of
    them existing counts as "install present".
    """
    lib_dir = install_dir / "lib"
    if is_windows():
        # Import lib is `usd_ms.lib`; DLL is `usd_ms.dll`.
        # `idtx-usd-sys/build.rs` links against the import lib.
        return [lib_dir / "usd_ms.lib", lib_dir / "usd_ms.dll"]
    elif is_macos():
        return [lib_dir / "libusd_ms.dylib"]
    else:
        return [lib_dir / "libusd_ms.so"]


def primary_monolithic_lib(install_dir: Path) -> Path:
    """The 'expected' library filename for user-facing messages."""
    return monolithic_lib_candidates(install_dir)[0]


def monolithic_lib_present(install_dir: Path) -> bool:
    return any(p.exists() for p in monolithic_lib_candidates(install_dir))


def find_python() -> str:
    """Return the python interpreter to invoke the upstream build script."""
    for candidate in ("python3", "python"):
        if shutil.which(candidate) is None:
            continue
        try:
            subprocess.run(
                [candidate, "--version"],
                check=True,
                capture_output=True,
                text=True,
            )
            return candidate
        except (subprocess.CalledProcessError, FileNotFoundError):
            continue
    sys.exit("ERROR: no python3 / python interpreter found on PATH")


# ---------------------------------------------------------------------------
# Prerequisite checks
# ---------------------------------------------------------------------------

def check_prerequisites() -> list:
    """Return a list of missing prerequisites (empty list = all good)."""
    missing: list = []
    if sys.version_info < (3, 8):
        missing.append(
            f"python >= 3.8 (found {sys.version_info.major}.{sys.version_info.minor})"
        )
    for tool in ("cmake", "git"):
        if shutil.which(tool) is None:
            missing.append(tool)

    if is_windows():
        # `cl.exe` only lives on PATH inside a Developer Command Prompt;
        # we prime it automatically via `vcvars64.bat` further down.  If
        # neither `cl.exe` is on PATH nor a VS installation is
        # discoverable, we surface that here.
        if shutil.which("cl.exe") is None and _find_vs_install_path() is None:
            missing.append(
                "MSVC toolchain -- install Visual Studio 2019+ with the "
                "'Desktop development with C++' workload"
            )
    else:
        if shutil.which("c++") is None and shutil.which("clang++") is None:
            missing.append("C++17 compiler (clang++ or g++)")

    return missing


# ---------------------------------------------------------------------------
# MSVC environment auto-detection (Windows only)
# ---------------------------------------------------------------------------
#
# OpenUSD's upstream build script invokes the system C++ compiler via
# CMake.  On Windows that means MSVC, which is only on PATH inside a
# "Developer Command Prompt for VS".  We locate VS via `vswhere.exe`,
# run `vcvars64.bat`, capture the resulting environment, and inject it
# into the subprocess we launch.  This mirrors
# `crates/idtx-usd-sys/build_support/msvc.rs` so the two entry points
# behave symmetrically.

def _vswhere_path() -> Optional[Path]:
    pf86 = os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")
    candidate = Path(pf86) / "Microsoft Visual Studio" / "Installer" / "vswhere.exe"
    return candidate if candidate.is_file() else None


def _find_vs_install_path() -> Optional[Path]:
    """Locate the newest VS 2017+ installation with the C++ workload."""
    if not is_windows():
        return None
    vswhere = _vswhere_path()
    if vswhere is None:
        return None
    try:
        out = subprocess.check_output(
            [
                str(vswhere),
                "-latest",
                "-products", "*",
                "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                "-property", "installationPath",
                "-format", "value",
            ],
            text=True,
        ).strip()
    except subprocess.CalledProcessError:
        return None
    return Path(out) if out else None


def get_msvc_environment() -> Optional[dict]:
    """
    Return an environment dict for `subprocess.run(env=...)` with PATH,
    INCLUDE, LIB, LIBPATH, etc. populated for MSVC x64.  Returns None if
    not on Windows or no VS install was found.
    """
    if not is_windows():
        return None

    vs_path = _find_vs_install_path()
    if vs_path is None:
        print(
            "[openusd] WARNING: could not locate Visual Studio via vswhere -- "
            "OpenUSD build will rely on the caller's PATH for the compiler."
        )
        return None

    vcvars = vs_path / "VC" / "Auxiliary" / "Build" / "vcvars64.bat"
    if not vcvars.is_file():
        print(f"[openusd] WARNING: {vcvars} not found; cannot prime MSVC env.")
        return None

    print(f"[openusd] priming MSVC environment via {vcvars}")
    # Robust quoting: rather than fighting cmd.exe's stacked
    # quote-stripping rules, we write a one-shot helper `.bat` to a temp
    # file and let cmd parse it as an ordinary batch script.
    sep = "=== IDTX_MSVC_ENV_BEGIN ==="
    bat_lines = [
        "@echo off",
        f'call "{vcvars}" >NUL',
        "if errorlevel 1 exit /b %errorlevel%",
        f"echo {sep}",
        "set",
    ]
    with tempfile.NamedTemporaryFile(
        prefix="idtx_msvc_",
        suffix=".bat",
        mode="w",
        delete=False,
        encoding="utf-8",
    ) as bat:
        bat.write("\r\n".join(bat_lines) + "\r\n")
        bat_path = bat.name

    try:
        completed = subprocess.run(
            ["cmd.exe", "/c", bat_path],
            capture_output=True,
            text=True,
            shell=False,
        )
    finally:
        try:
            os.unlink(bat_path)
        except OSError:
            pass

    if completed.returncode != 0:
        print(
            "[openusd] WARNING: vcvars64.bat returned non-zero; "
            f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
        return None

    # Anything before the separator is pre-vcvars output; discard it.
    lines = completed.stdout.splitlines()
    if sep in lines:
        env_lines = lines[lines.index(sep) + 1:]
    else:
        env_lines = lines  # vcvars produced no banner; fall back.

    env: dict = {}
    for line in env_lines:
        if "=" in line:
            key, _, value = line.partition("=")
            env[key] = value

    if "VCToolsInstallDir" not in env and "VCINSTALLDIR" not in env:
        print("[openusd] WARNING: MSVC env did not include VCToolsInstallDir.")
        return None
    return env


# ---------------------------------------------------------------------------
# Version marker (records the last successful build's version in the
# install prefix; lets subsequent invocations skip a full rebuild when
# an install matching the requested version already exists).
# ---------------------------------------------------------------------------

def read_version_marker(install_dir: Path) -> Optional[str]:
    marker = install_dir / VERSION_MARKER
    if not marker.is_file():
        return None
    try:
        return marker.read_text(encoding="utf-8").strip()
    except OSError:
        return None


def write_version_marker(install_dir: Path, version: str) -> None:
    marker = install_dir / VERSION_MARKER
    try:
        marker.write_text(version + "\n", encoding="utf-8")
    except OSError as e:
        print(f"[openusd] WARNING: could not write {marker}: {e}")


# ---------------------------------------------------------------------------
# Steps
# ---------------------------------------------------------------------------

def clone(src_dir: Path, version: str) -> None:
    if src_dir.exists() and (src_dir / ".git").exists():
        print(f"[openusd] source already cloned at {src_dir}")
        return
    if src_dir.exists() and any(src_dir.iterdir()):
        # Non-empty non-git dir: refuse to clobber, and refuse to
        # silently proceed -- the user probably pointed us at the wrong
        # tree.
        sys.exit(
            f"ERROR: {src_dir} exists and is non-empty but is not a git "
            f"clone.\n       Either remove it, or pass --src-dir pointing "
            f"at an existing OpenUSD checkout."
        )

    src_dir.parent.mkdir(parents=True, exist_ok=True)
    tag = f"v{version}"
    print(f"[openusd] cloning OpenUSD {tag} -> {src_dir}")
    rc = subprocess.run(
        [
            "git", "clone",
            "-b", tag,
            "--recursive",
            "--depth", "2",
            "https://github.com/PixarAnimationStudios/OpenUSD.git",
            str(src_dir),
        ]
    ).returncode
    if rc != 0:
        sys.exit(f"ERROR: git clone failed (exit code {rc})")


def build(
    src_dir: Path,
    install_dir: Path,
    variant: str,
    extra_cmake: str,
) -> None:
    install_dir.mkdir(parents=True, exist_ok=True)

    upstream = src_dir / "build_scripts" / "build_usd.py"
    if not upstream.is_file():
        sys.exit(
            f"ERROR: cannot find {upstream}.\n"
            f"       Is {src_dir} a valid OpenUSD checkout?"
        )

    variant_map = {
        "release": "release",
        "debug": "debug",
        "relwithdebinfo": "relwithdebuginfo",
    }
    if variant not in variant_map:
        sys.exit(
            f"ERROR: unknown variant '{variant}'. Use one of {list(variant_map)}"
        )

    python_cmd = find_python()
    cmake_args = "-DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_CXX_STANDARD=17"
    if extra_cmake:
        cmake_args += " " + extra_cmake

    cmd = [
        python_cmd,
        str(upstream),
        str(install_dir),
        "--verbose",
        "--build-variant", variant_map[variant],
        "--build-monolithic",
        "--no-python",
        "--no-examples",
        "--no-tutorials",
        "--tools",
        "--no-debug-python",
        "--no-openvdb",
        "--no-usdview",
        "--no-imaging",
        "--no-vulkan",
        "--materialx",
        "--onetbb",
        "--cmake-build-args", cmake_args,
    ]

    # Start with our current environment and overlay MSVC vars on top.
    build_env = os.environ.copy()
    if is_windows():
        msvc_env = get_msvc_environment()
        if msvc_env is not None:
            # Preserve caller-provided vars (e.g. CMAKE_GENERATOR
            # overrides) while letting MSVC values win for PATH/INCLUDE/LIB.
            merged = build_env.copy()
            merged.update(msvc_env)
            build_env = merged

    print(f"[openusd] building OpenUSD ({variant}) -> {install_dir}")
    print(f"[openusd] cmd: {' '.join(cmd)}")
    rc = subprocess.run(cmd, env=build_env).returncode
    if rc != 0:
        sys.exit(f"ERROR: OpenUSD build failed (exit code {rc})")


# ---------------------------------------------------------------------------
# Reuse fast path
# ---------------------------------------------------------------------------

def try_reuse_existing_install(install_dir: Path, version: str) -> bool:
    """
    Return True if `install_dir` already contains a compatible OpenUSD
    install and the caller can skip clone+build entirely.

    Compatibility rules:
      * `<install-dir>/lib/<usd_ms>` must exist for the current platform.
      * `<install-dir>/include` must exist (headers required by the shim).
      * If a `.idtx_openusd_version` marker is present it must match the
        requested version.  If no marker is present we assume the user
        pointed us at a hand-managed install and accept it with a
        warning -- they are on the hook for version compatibility.
    """
    if not install_dir.is_dir():
        return False
    if not monolithic_lib_present(install_dir):
        return False
    if not (install_dir / "include").is_dir():
        return False

    recorded = read_version_marker(install_dir)
    if recorded is None:
        print(
            f"[openusd] found existing install at {install_dir} with no "
            f"version marker; reusing (assumed OpenUSD {version})."
        )
        # Drop a marker now so future runs treat this as blessed.
        write_version_marker(install_dir, version)
        return True

    if recorded != version:
        print(
            f"[openusd] existing install at {install_dir} was built for "
            f"OpenUSD {recorded}, but {version} was requested -- will "
            f"rebuild.  Pass --force to always rebuild, or point "
            f"--install-dir at a different location to preserve the "
            f"existing tree."
        )
        return False

    print(
        f"[openusd] reusing existing OpenUSD {recorded} install at "
        f"{install_dir}"
    )
    return True


# ---------------------------------------------------------------------------
# Plan printer (--check-only)
# ---------------------------------------------------------------------------

def print_plan(
    version: str,
    src_dir: Path,
    install_dir: Path,
    variant: str,
) -> None:
    print("=" * 70)
    print("OpenUSD build plan (idtx-usd-sys)")
    print("=" * 70)
    print(f"  Version pin      : OpenUSD {version}  (ADR-0003)")
    print(f"  Source tree      : {src_dir}")
    print(f"  Install prefix   : {install_dir}")
    print(f"  Build variant    : {variant}")
    print(f"  Monolithic build : yes  (--build-monolithic)")
    print()
    if monolithic_lib_present(install_dir):
        recorded = read_version_marker(install_dir) or "<no marker>"
        print(f"  Existing install : PRESENT (version marker: {recorded})")
    else:
        print("  Existing install : NOT FOUND -- clone + build required")
    print()
    print("Downstream:")
    print("  * `crates/idtx-usd-sys/build.rs` reads $IDTX_USD_DIR, falling")
    print("    back to `<workspace>/thirdparty/openusd-26.08-mtlx/install`.")
    print(f"  * Export IDTX_USD_DIR={install_dir} to align them.")
    print("=" * 70)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> int:
    cfg = load_config()

    parser = argparse.ArgumentParser(
        description=(
            "Clone and build (or reuse) a monolithic OpenUSD install for "
            "the IDTX C++ shim."
        ),
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument(
        "--src-dir",
        default=str(WORKSPACE_ROOT / cfg["src_dir"]),
        help="Where to clone/find OpenUSD sources.",
    )
    parser.add_argument(
        "--install-dir", "--install-prefix",
        dest="install_dir",
        default=str(WORKSPACE_ROOT / cfg["install_dir"]),
        help=(
            "Where OpenUSD is/should be installed.  If this directory "
            "already contains a compatible `usd_ms` library the clone "
            "and build steps are skipped."
        ),
    )
    parser.add_argument(
        "--version", "--pin",
        dest="version",
        default=cfg["version"],
        help=(
            "OpenUSD tag to clone/verify (without the leading 'v').  "
            "Authoritative default comes from openusd_config.toml."
        ),
    )
    parser.add_argument(
        "--variant",
        default="release",
        choices=["release", "debug", "relwithdebinfo"],
        help="CMake build variant.",
    )
    parser.add_argument(
        "--extra-cmake",
        default="",
        help="Extra args appended to --cmake-build-args.",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Rebuild even if a compatible install already exists.",
    )
    parser.add_argument(
        "--check-only",
        action="store_true",
        help=(
            "Verify prerequisites and print the intended plan, but do "
            "not clone or build."
        ),
    )
    args = parser.parse_args()

    src_dir = Path(args.src_dir).resolve()
    install_dir = Path(args.install_dir).resolve()

    print(f"[openusd] workspace root : {WORKSPACE_ROOT}")
    print(f"[openusd] version        : {args.version}")
    print(f"[openusd] source dir     : {src_dir}")
    print(f"[openusd] install dir    : {install_dir}")
    print(f"[openusd] variant        : {args.variant}")

    # 1. Environment check.
    missing = check_prerequisites()
    if missing:
        print("Missing prerequisites:", file=sys.stderr)
        for m in missing:
            print(f"  - {m}", file=sys.stderr)
        print(
            "\nInstall the tools above, then re-run this script.",
            file=sys.stderr,
        )
        return 2

    # 2. --check-only: print plan and exit.
    if args.check_only:
        print_plan(args.version, src_dir, install_dir, args.variant)
        return 0

    # 3. Reuse fast path.
    if not args.force and try_reuse_existing_install(install_dir, args.version):
        lib = primary_monolithic_lib(install_dir)
        print(f"[openusd] DONE. Library at {lib}")
        print(
            f"[openusd] point `crates/idtx-usd-sys` at this install via:"
        )
        print(f"           export IDTX_USD_DIR={install_dir}")
        return 0

    # 4. Clone + build.
    clone(src_dir, args.version)
    build(src_dir, install_dir, args.variant, args.extra_cmake)

    # 5. Verify + record.
    if not monolithic_lib_present(install_dir):
        sys.exit(
            f"ERROR: expected monolithic library not found after build "
            f"under {install_dir / 'lib'}.  Inspect that directory for "
            f"the actual produced artifact."
        )
    write_version_marker(install_dir, args.version)

    lib = primary_monolithic_lib(install_dir)
    print(f"[openusd] DONE. Library at {lib}")
    print(f"[openusd] point `crates/idtx-usd-sys` at this install via:")
    print(f"           export IDTX_USD_DIR={install_dir}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
