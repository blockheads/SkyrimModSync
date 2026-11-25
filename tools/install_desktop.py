"""Install a packaged Linux application/file association into user-owned directories."""
import argparse
import os
import subprocess
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("executable", type=Path)
parser.add_argument("--data-home", type=Path, default=Path(os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local/share"))))
args = parser.parse_args()
executable = args.executable.resolve()
if not executable.is_file() or not os.access(executable, os.X_OK):
    parser.error("Select the packaged executable, not its directory.")
applications = args.data_home / "applications"
mime = args.data_home / "mime/packages"
applications.mkdir(parents=True, exist_ok=True)
mime.mkdir(parents=True, exist_ok=True)
quoted = str(executable).replace("\\", "\\\\").replace('"', '\\"').replace("`", "\\`").replace("$", "\\$")
(applications / "skyrim-mod-sync.desktop").write_text(f'''[Desktop Entry]
Type=Application
Name=Skyrim Mod Sync
Comment=Install a shared, verified Skyrim setup
Exec="{quoted}" gui %f
Terminal=false
Categories=Game;
MimeType=application/x-skyrimsync;
''')
(mime / "skyrim-mod-sync.xml").write_text('''<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="application/x-skyrimsync">
    <comment>Shared Skyrim setup</comment>
    <glob pattern="*.skyrimsync"/>
  </mime-type>
</mime-info>
''')
for command in (["update-mime-database", str(args.data_home / "mime")],
                ["update-desktop-database", str(applications)]):
    try:
        subprocess.run(command, check=True)
    except FileNotFoundError:
        print("Optional desktop cache updater unavailable:", command[0])
print("Installed desktop entry:", applications / "skyrim-mod-sync.desktop")
