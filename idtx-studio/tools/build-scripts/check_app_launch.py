"""Build one remote-enabled product and launch its real executable without loader overrides."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument("app", choices=["inspector", "poser", "conductor", "runtime"])
args = parser.parse_args()
workspace = Path(__file__).resolve().parents[2]
features = "remote,metrics" if args.app == "runtime" else "remote,idtx-studio-shell/metrics"
subprocess.run(["cargo", "build", "--locked", "-p", "idtx-" + args.app, "--features", features], cwd=workspace, check=True)
executable = workspace / "target/debug" / ("idtx-" + args.app + (".exe" if os.name == "nt" else ""))
environment = os.environ.copy()
for key in ("IDTX_USD_DIR", "DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH", "LD_LIBRARY_PATH", "PXR_PLUGINPATH_NAME"):
    environment.pop(key, None)
with tempfile.TemporaryDirectory(prefix="idtx-app-launch-") as temporary:
    directory = Path(temporary)
    stage = directory / "probe.usda"
    stage.write_text('#usda 1.0\n(defaultPrim = "World")\ndef Xform "World" {\n def Sphere "Ball" {}\n}\n')
    command = [str(executable)]
    if args.app == "runtime":
        image = directory / "render.png"
        command += ["--stage", str(stage), "--headless", "--frames", "3", "--screenshot", str(image)]
    else:
        environment["IDTX_STARTUP_PROBE_STAGE"] = str(stage)
    result = subprocess.run(command, cwd=directory, env=environment, capture_output=True, text=True, timeout=90)
    print(result.stdout)
    print(result.stderr)
    result.check_returncode()
    if args.app == "runtime":
        assert image.read_bytes().startswith(b"\x89PNG\r\n\x1a\n"), "runtime did not render"
    else:
        assert "idtx startup probe:" in result.stdout, "product did not complete its USD startup probe"
