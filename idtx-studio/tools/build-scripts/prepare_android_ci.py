"""Install the PR gate's pinned NDK and export its location to later CI steps."""
import os
from pathlib import Path
import shutil
import subprocess

sdk = Path(os.environ.get("ANDROID_HOME", Path.home() / "Library/Android/sdk"))
manager = shutil.which("sdkmanager")
if manager is None:
    candidates = [sdk / "cmdline-tools/latest/bin/sdkmanager", Path("/opt/homebrew/bin/sdkmanager"), Path("/usr/local/bin/sdkmanager")]
    manager = next((str(path) for path in candidates if path.is_file()), None)
if manager is None:
    raise SystemExit("Android command-line tools are required")
subprocess.run([manager, "--sdk_root=" + str(sdk), "--licenses"], input="y\n" * 100, text=True, check=True)
subprocess.run([manager, "--sdk_root=" + str(sdk), "ndk;30.0.16248370"], input="y\n" * 100, text=True, check=True)
with open(os.environ["GITHUB_ENV"], "a") as output:
    output.write(f"ANDROID_HOME={sdk}\nANDROID_SDK_ROOT={sdk}\nANDROID_NDK_ROOT={sdk / 'ndk/30.0.16248370'}\n")
