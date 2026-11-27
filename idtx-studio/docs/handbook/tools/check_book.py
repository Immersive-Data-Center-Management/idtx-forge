"""Check handbook navigation, local links, fences and mdBook configuration."""
from pathlib import Path
import re
import sys
import tomllib
from urllib.parse import unquote, urlsplit


def without_fences(text: str, file: Path, errors: list[str]) -> str:
    visible = []
    marker = None
    for number, line in enumerate(text.splitlines(), 1):
        match = re.match(r"^\s*(`{3,}|~{3,})", line)
        if match:
            candidate = match.group(1)
            if marker is None:
                marker = candidate
            elif candidate[0] == marker[0] and len(candidate) >= len(marker):
                marker = None
            continue
        if marker is None:
            visible.append(line)
    if marker:
        errors.append(f"{file}: unclosed {marker} fence")
    return "\n".join(visible)


def check_listings(root: Path, pages: list[Path], errors: list[str]) -> int:
    """Keep downloadable projects identical to their complete Markdown listings."""
    folders = {
        "example-file": root / "examples/signal-room",
        "animation-routing-file": root / "examples/animation-routing",
        "shared-modules-file": root / "examples/shared-modules",
    }
    seen: set[Path] = set()
    for page in pages:
        lines = page.read_text(encoding="utf-8").splitlines(keepends=True)
        index = 0
        while index < len(lines):
            marker = re.fullmatch(r"<!-- (example-file|animation-routing-file|shared-modules-file): ([^>]+) -->\s*", lines[index])
            index += 1
            if marker is None:
                continue
            folder = folders[marker.group(1)]
            name = marker.group(2).strip()
            target = (folder / name).resolve()
            if not target.is_relative_to(folder.resolve()) or target in seen:
                errors.append(f"{page.relative_to(root)}: invalid/duplicate project listing {name}")
                continue
            seen.add(target)
            while index < len(lines) and not lines[index].strip():
                index += 1
            opening = re.match(r"^(\x60{3,})", lines[index]) if index < len(lines) else None
            if opening is None:
                errors.append(f"{page.relative_to(root)}: missing fenced listing for {name}")
                continue
            fence = opening.group(1)
            index += 1
            start = index
            while index < len(lines) and lines[index].strip() != fence:
                index += 1
            if index == len(lines):
                errors.append(f"{page.relative_to(root)}: unclosed project listing {name}")
            elif not target.is_file():
                errors.append(f"{page.relative_to(root)}: missing companion file {name}")
            elif "".join(lines[start:index]) != target.read_text(encoding="utf-8"):
                errors.append(f"{page.relative_to(root)}: listing differs from companion file {name}")
            index += 1
    return len(seen)


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    errors: list[str] = []
    config = tomllib.loads((root / "book.toml").read_text(encoding="utf-8"))
    source = root / config["book"]["src"]
    summary = source / "SUMMARY.md"
    if not summary.is_file():
        errors.append("Missing src/SUMMARY.md")
    pages = [root / "README.md", *source.rglob("*.md"), *(root / "examples").rglob("*.md")]
    listing_count = check_listings(root, pages, errors)
    anchors: dict[Path, set[str]] = {}
    for file in pages:
        visible = without_fences(file.read_text(encoding="utf-8"), file, [])
        found: set[str] = set()
        duplicates: dict[str, int] = {}
        for heading in re.findall(r"^#{1,6}\s+(.+)$", visible, re.MULTILINE):
            heading = re.sub(r"\[([^]]+)\]\([^)]+\)", r"\1", heading)
            slug = re.sub(r"[^\w\s-]", "", heading.lower()).strip().replace(" ", "-")
            suffix = duplicates.get(slug, 0)
            duplicates[slug] = suffix + 1
            found.add(f"{slug}-{suffix}" if suffix else slug)
        anchors[file.resolve()] = found
    link_count = 0
    for file in pages:
        text = without_fences(file.read_text(encoding="utf-8"), file, errors)
        for match in re.finditer(r"!?\[[^\]]*\]\((<[^>]+>|[^)]+)\)", text):
            raw = match.group(1).strip()
            destination = raw[1:-1] if raw.startswith("<") else raw.split(' "', 1)[0]
            parsed = urlsplit(destination)
            if parsed.scheme or parsed.netloc:
                continue
            link_count += 1
            target = (file.parent / unquote(parsed.path)).resolve() if parsed.path else file.resolve()
            if not target.is_relative_to(root):
                errors.append(f"{file.relative_to(root)}: link escapes the standalone book: {destination}")
            elif not target.exists():
                errors.append(f"{file.relative_to(root)}: missing link {destination}")
            elif parsed.fragment and target in anchors and unquote(parsed.fragment) not in anchors[target]:
                errors.append(f"{file.relative_to(root)}: missing heading {destination}")
    if summary.is_file():
        navigation = without_fences(summary.read_text(encoding="utf-8"), summary, errors)
        referenced = {
            (source / unquote(urlsplit(match.group(1)).path)).resolve()
            for match in re.finditer(r"\[[^\]]+\]\(([^)]+)\)", navigation)
        }
        for file in source.rglob("*.md"):
            if file != summary and file.resolve() not in referenced:
                errors.append(f"Page absent from SUMMARY.md: {file.relative_to(source)}")
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        return 1
    print(f"OK: {len(pages)} Markdown files, {link_count} local links, {listing_count} matching project listings, balanced fences and complete navigation")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
