"""Machine-layout fixtures and explicit, reproducible environment gaps.

No Steam client, Nexus account or game process is used. These cases exercise the same code paths used by the CLI.
"""
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from skyrim_mod_sync.discovery import discover
from skyrim_mod_sync.isolation import engine_context
from skyrim_mod_sync.paths import SyncError


def steam_root(path):
    (path / 'steamapps').mkdir(parents=True)
    return path


def game_in(library):
    apps = library / 'steamapps'
    game = apps / 'common/Skyrim Special Edition'
    game.mkdir(parents=True)
    (game / 'SkyrimSE.exe').write_bytes(b'fixture executable, never launched')
    (apps / 'appmanifest_489830.acf').write_text('"AppState" { "installdir" "Skyrim Special Edition" }')
    return game


def libraries(steam, *paths):
    entries = ' '.join(f'"{i}" {{ "path" "{str(path).replace(chr(92), chr(92) * 2)}" }}'
                       for i, path in enumerate(paths))
    (steam / 'steamapps/libraryfolders.vdf').write_text('"libraryfolders" { ' + entries + ' }')


@pytest.mark.parametrize('relative', ['.steam/steam', '.steam/debian-installation', '.local/share/Steam'])
def test_native_steam_roots(relative, tmp_path):
    home = tmp_path / 'home'
    steam = steam_root(home / relative)
    game = game_in(steam)
    assert discover(home)['base'] == game.resolve()


@pytest.mark.skipif(os.name == 'nt', reason='Linux home symlink layout')
def test_native_aliases_deduplicate_the_same_game(tmp_path):
    home = tmp_path / 'home'
    steam = steam_root(home / '.local/share/Steam')
    game = game_in(steam)
    (home / '.steam').mkdir()
    (home / '.steam/steam').symlink_to(steam, target_is_directory=True)
    (home / '.steam/debian-installation').symlink_to(steam, target_is_directory=True)
    assert discover(home)['base'] == game.resolve()


@pytest.mark.parametrize('name', ['External drive', '朋友的游戏', '$(must-stay-literal)'])
def test_external_library_paths_are_read_as_data(name, tmp_path):
    home = tmp_path / 'home'
    steam = steam_root(home / '.local/share/Steam')
    external = tmp_path / name
    game = game_in(external)
    libraries(steam, external)
    assert discover(home)['base'] == game.resolve()


def test_custom_steam_path_works_when_supplied_to_discovery(tmp_path):
    steam = steam_root(tmp_path / 'custom Steam')
    game = game_in(steam)
    assert discover(tmp_path / 'empty-home', steam=steam)['base'] == game.resolve()


def test_distinct_game_installs_require_selection(tmp_path):
    home = tmp_path / 'home'
    steam = steam_root(home / '.local/share/Steam')
    game_in(steam)
    other = tmp_path / 'other-library'
    game_in(other)
    libraries(steam, other)
    with pytest.raises(SyncError) as error:
        discover(home)
    assert error.value.code == 'game_discovery'
    assert 'Multiple' in str(error.value)


def test_disconnected_secondary_library_does_not_hide_local_game(tmp_path):
    home = tmp_path / 'home'
    steam = steam_root(home / '.local/share/Steam')
    game = game_in(steam)
    libraries(steam, tmp_path / 'unmounted-drive')
    assert discover(home)['base'] == game.resolve()


def test_game_without_manifest_is_not_assumed_installed(tmp_path):
    home = tmp_path / 'home'
    steam = steam_root(home / '.local/share/Steam')
    game_in(steam)
    (steam / 'steamapps/appmanifest_489830.acf').unlink()
    with pytest.raises(SyncError, match='Cannot locate'):
        discover(home)


def test_missing_launch_tools_are_reported_as_missing(tmp_path):
    home = tmp_path / 'home'
    steam = steam_root(home / '.local/share/Steam')
    game_in(steam)
    found = discover(home)
    assert found['proton'] is None
    assert found['runtime'] is None


def test_flatpak_layout_is_detected(tmp_path):
    home = tmp_path / 'home'
    steam = steam_root(home / '.var/app/com.valvesoftware.Steam/.local/share/Steam')
    game = game_in(steam)
    assert discover(home)['base'] == game.resolve()


def test_broken_library_metadata_keeps_the_primary_game_candidate(tmp_path):
    home = tmp_path / 'home'
    steam = steam_root(home / '.local/share/Steam')
    game = game_in(steam)
    (steam / 'steamapps/libraryfolders.vdf').write_text('"libraryfolders" { "0" {')
    assert discover(home)['base'] == game.resolve()


@pytest.mark.skipif(sys.platform != 'linux', reason='Actual Linux filesystem namespace probe')
def test_discovered_home_library_is_visible_to_isolated_engine(tmp_path, monkeypatch):
    if not shutil.which('bwrap'):
        pytest.skip('bubblewrap is not installed on this runner')
    home = tmp_path / 'fake-user-home'
    steam = steam_root(home / '.local/share/Steam')
    library = home / 'Games/secondary-library'
    game = game_in(library)
    libraries(steam, library)
    assert discover(home)['base'] == game.resolve()
    root = tmp_path / 'managed'
    job = root / 'code-jobs/namespace-fixture'
    job.mkdir(parents=True)
    monkeypatch.setattr(Path, 'home', classmethod(lambda cls: home))
    prefix, environment = engine_context(root, job, steam, None, None, base=game)
    command = prefix + [sys.executable, '-c',
                        'import pathlib,sys,json; print(json.dumps({"base_visible": pathlib.Path(sys.argv[1]).is_file()}))',
                        str(game / 'SkyrimSE.exe')]
    result = subprocess.run(command, env=environment, capture_output=True, text=True, timeout=10)
    if result.returncode:
        pytest.skip('This runner cannot create the Linux namespace: ' + result.stderr[:160])
    assert json.loads(result.stdout)['base_visible'], 'Discovery succeeded, but the real engine namespace hides the base game.'


def test_missing_namespace_tool_refuses_before_writing(tmp_path, monkeypatch):
    monkeypatch.setattr('skyrim_mod_sync.isolation.shutil.which', lambda _: None)
    root = tmp_path / 'managed'
    with pytest.raises(SyncError, match='bubblewrap'):
        engine_context(root, root / 'job', tmp_path / 'steam', None, None)
    assert not root.exists()


@pytest.mark.skipif(os.name == 'nt', reason='XDG storage is a Unix convention')
@pytest.mark.parametrize('value', ['', 'relative-data'])
def test_invalid_xdg_data_home_uses_default_storage(value, tmp_path, monkeypatch):
    from skyrim_mod_sync.cli import default_storage
    home = tmp_path / 'fake-user-home'
    monkeypatch.setattr(Path, 'home', classmethod(lambda cls: home))
    monkeypatch.setenv('XDG_DATA_HOME', value)
    assert default_storage() == home / '.local/share/skyrim-mod-sync'
