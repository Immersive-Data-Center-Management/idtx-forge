"""Reconstruct Signal Room from the handbook's complete file listings.

Only Python's standard library is needed. Existing files are never overwritten.
"""
from pathlib import Path
import argparse
import re
import sys


FILES = (
    "README.md", "AGENTS.md", ".gitignore", "project.toml", "input_map.toml",
    "assets/station.usda", "levels/hub.usda", "levels/lab.usda",
    "behaviors/console.rs", "behaviors/pointer.rs",
    "extensions/specimen.usda", "extensions/spawn-demo.usda",
    "extensions/spawn_demo.rs",
)


def read_listings(book: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    for page in sorted((book / "src" / "example").glob("*.md")):
        lines = page.read_text(encoding="utf-8").splitlines(keepends=True)
        index = 0
        while index < len(lines):
            marker = re.fullmatch(r"<!-- example-file: ([^\r\n]+) -->\s*", lines[index])
            index += 1
            if marker is None:
                continue
            name = marker.group(1)
            if name not in FILES or name in result:
                raise ValueError(f"{page.name}: unexpected or duplicate file {name}")
            while index < len(lines) and not lines[index].strip():
                index += 1
            opening = re.fullmatch(r"(`{3,})[^\r\n]*\s*", lines[index]) if index < len(lines) else None
            if opening is None:
                raise ValueError(f"{page.name}: {name} has no complete fenced listing")
            fence = opening.group(1)
            index += 1
            start = index
            while index < len(lines) and lines[index].strip() != fence:
                index += 1
            if index == len(lines):
                raise ValueError(f"{page.name}: unclosed listing for {name}")
            result[name] = "".join(lines[start:index])
            index += 1
    missing = set(FILES) - set(result)
    if missing:
        raise ValueError(f"Missing complete listings: {', '.join(sorted(missing))}")
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", nargs="?", type=Path, help="New or existing project directory; authored files must not exist")
    parser.add_argument("--check", action="store_true", help="Check listings against the companion project without writing files")
    args = parser.parse_args()
    if args.check and args.destination is not None:
        parser.error("--check does not take a destination")
    if not args.check and args.destination is None:
        parser.error("choose a destination, or pass --check")
    book = Path(__file__).resolve().parents[1]
    try:
        listings = read_listings(book)
        if args.check:
            companion = book / "examples" / "signal-room"
            for name in FILES:
                source = companion / name
                if source.read_text(encoding="utf-8") != listings[name]:
                    raise ValueError(f"Listing differs from companion file: {name}")
            print(f"OK: {len(FILES)} complete file listings match the portable project")
            return 0
        destination = args.destination.expanduser().resolve()
        targets = [destination / name for name in FILES]
        for target in targets:
            if target.exists() or target.is_symlink():
                raise ValueError(f"Refusing to overwrite existing path: {target}")
            if not target.resolve().is_relative_to(destination):
                raise ValueError(f"Project path escapes destination: {target}")
        for name, target in zip(FILES, targets):
            target.parent.mkdir(parents=True, exist_ok=True)
            with target.open("x", encoding="utf-8", newline="\n") as output:
                output.write(listings[name])
        print(f"Created {len(FILES)} files in {destination}")
        print("Launch packaged IDTX Conductor and use Project > Open Project… to select project.toml.")
        return 0
    except (OSError, ValueError) as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
