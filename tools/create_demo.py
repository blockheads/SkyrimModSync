"""Create an account-free synthetic setup for reviewing the native UI/CLI."""
import argparse
import json
from pathlib import Path
from zipfile import ZipFile

from skyrim_mod_sync.exporter import export_setup

parser = argparse.ArgumentParser()
parser.add_argument("directory", type=Path)
args = parser.parse_args()
root = args.directory.resolve()
root.mkdir(parents=True, exist_ok=False)
base = root / "base"
installed = root / "host-list"
def put(directory, name, data):
    file = directory / name
    file.parent.mkdir(parents=True, exist_ok=True)
    file.write_bytes(data)
put(base, "SkyrimSE.exe", b"Synthetic fixture, NOT a playable executable.")
put(base, "Data/Skyrim.esm", b"Synthetic fixture, NOT game data.")
put(installed, "Game Folder Files/skse64_loader.exe", b"Synthetic fixture, NOT a loader.")
put(installed, "mods/Low/textures/demo.dds", b"lower priority")
put(installed, "mods/High/textures/demo.dds", b"winning priority")
put(installed, "mods/High/Demo.esp", b"Synthetic plugin.")
put(installed, "profiles/Demo/modlist.txt", b"+High\n+Low\n")
put(installed, "profiles/Demo/plugins.txt", b"*Demo.esp\n")
put(installed, "profiles/Demo/Skyrim.ini", b"[General]\nsLanguage=ENGLISH\n")
artifact = root / "demo.wabbajack"
with ZipFile(artifact, "w") as archive:
    archive.writestr("modlist", json.dumps({"Name": "Offline synthetic demo", "GameType": "SkyrimSpecialEdition", "Archives": [], "Directives": []}))
package = root / "Synthetic-Demo.skyrimsync"
export_setup(base=base, installed=installed, artifact=artifact, output=package,
             name="Synthetic test setup — not a playable game", profile="Demo")
print(json.dumps({"package": str(package), "base": str(base), "reconstructed": str(installed),
                  "managed_storage": str(root / "managed"),
                  "note": "Use the reconstructed-directory option; this fixture downloads nothing and cannot launch Skyrim."}, indent=2))
