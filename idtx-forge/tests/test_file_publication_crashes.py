"""Run with the standalone file_publication_crash probe path as the argument."""
import pathlib
import subprocess
import sys
import tempfile

probe = pathlib.Path(sys.argv[1]).resolve()
for extension in (".usd", ".usdc", ".usdz"):
    for checkpoint in ("before-replace", "after-replace", "after-backup", "after-restore"):
        with tempfile.TemporaryDirectory(prefix="idtx-publication-crash-") as directory:
            root = pathlib.Path(directory)
            target = root / ("scene" + extension)
            original, replacement = b"a" * 64000, b"b" * 96000
            target.write_bytes(original)
            result = subprocess.run([str(probe), str(target), checkpoint], timeout=20)
            assert result.returncode == 86, (extension, checkpoint, result.returncode)
            files = [p for p in root.rglob("*") if p.is_file()]
            bodies = [p.read_bytes() for p in files]
            assert original in bodies or replacement in bodies, (extension, checkpoint, files)
            assert all(b in (original, replacement) for b in bodies), (extension, checkpoint)
            if checkpoint == "after-replace":
                assert target.read_bytes() == replacement
            elif checkpoint == "after-backup":
                assert not target.exists() and original in bodies and replacement in bodies
            else:
                assert target.read_bytes() == original and replacement in bodies
            print(f"PASS {extension} {checkpoint}: a complete generation survives")
