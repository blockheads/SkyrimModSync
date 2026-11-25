import base64
import copy
import hashlib
import json
import shutil
import zlib
from pathlib import Path

import pytest

from skyrim_mod_sync import codes
from skyrim_mod_sync.auth import load_key, save_key, validate_key
from skyrim_mod_sync.discovery import discover, parse_vdf
from skyrim_mod_sync.exporter import export_setup
from skyrim_mod_sync.installer import install, verify
from skyrim_mod_sync.paths import SyncError
from skyrim_mod_sync.recipe import load
from skyrim_mod_sync.workflow import sync_code


def test_codes_are_pinned_portable_and_contain_no_local_paths():
    descriptor = codes.decode('SME-2.7.1')
    shared = {**descriptor, 'setup_id': 'a' * 64}
    assert codes.decode(codes.encode(shared)) == shared
    assert codes.job_id(shared) == codes.job_id(descriptor)
    assert len(codes.encode(shared)) < 650
    assert descriptor['sha256'] == '7dda0f7211df4184db1e2069052787d3dc3741084e1ea486cb54a3558e492216'


@pytest.mark.parametrize('change', [dict(url='https://example.com/modpack'), dict(profile='../outside'),
                                     dict(sha256='latest'), dict(engine='execute me')])
def test_bad_codes_are_rejected(change):
    data = {**codes.SME, **change}
    text = 'SMS1-' + base64.urlsafe_b64encode(zlib.compress(json.dumps(data).encode())).decode().rstrip('=')
    with pytest.raises(SyncError):
        codes.decode(text)


def test_oversized_compressed_code_is_rejected():
    value = 'SMS1-' + base64.urlsafe_b64encode(zlib.compress(b'x' * 100000)).decode()
    with pytest.raises(SyncError):
        codes.decode(value)


def test_nexus_key_storage_is_private_and_api_response_is_sanitized(tmp_path, monkeypatch):
    path = tmp_path / 'config/key'
    key = 'synthetic-personal-api-key'
    save_key(key, path)
    assert load_key(path=path) == key
    if __import__('os').name != 'nt':
        assert path.stat().st_mode & 0o777 == 0o600
    import io
    def response(request, **kwargs):
        assert request.get_header('Apikey') == key
        assert request.full_url == 'https://api.nexusmods.com/v1/users/validate.json'
        return io.BytesIO(json.dumps({'is_premium': True, 'email': 'private@example.com', 'key': key}).encode())
    monkeypatch.setattr('skyrim_mod_sync.auth.urlopen', response)
    report = validate_key(key)
    assert report == {'state': 'nexus_authenticated', 'premium': True}
    assert key not in json.dumps(report)


def test_discovery_finds_game_and_tools_across_steam_libraries(tmp_path):
    steam = tmp_path / '.steam/debian-installation'
    other = tmp_path / 'other library'
    for folder in [steam / 'steamapps', other / 'steamapps/common/Skyrim Special Edition',
                   other / 'steamapps/common/Proton 10.0', other / 'steamapps/common/SteamLinuxRuntime_sniper']:
        folder.mkdir(parents=True)
    (steam / 'steamapps/libraryfolders.vdf').write_text('"libraryfolders" { "0" { "path" "' + str(other) + '" } }')
    (other / 'steamapps/appmanifest_489830.acf').write_text('"AppState" { "installdir" "Skyrim Special Edition" }')
    (other / 'steamapps/common/Skyrim Special Edition/SkyrimSE.exe').write_bytes(b'fake')
    (other / 'steamapps/common/Proton 10.0/proton').write_bytes(b'fake')
    (other / 'steamapps/common/SteamLinuxRuntime_sniper/run').write_bytes(b'fake')
    found = discover(tmp_path)
    assert found['base'] == other / 'steamapps/common/Skyrim Special Edition'
    assert found['steam'] == steam
    assert found['proton'] == other / 'steamapps/common/Proton 10.0'
    assert found['runtime'] == other / 'steamapps/common/SteamLinuxRuntime_sniper'
    with pytest.raises(ValueError):
        parse_vdf('"unterminated" {')


def make_stock(setup):
    stock = setup['installed'] / 'Stock Game'
    stock.mkdir()
    for p in setup['base'].rglob('*'):
        if p.is_file():
            target = stock / p.relative_to(setup['base'])
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(p, target)
    # The loader is represented as a standard Root Builder mod.
    root = setup['installed'] / 'mods/High/root'
    root.mkdir()
    shutil.copyfile(setup['installed'] / 'Game Folder Files/skse64_loader.exe', root / 'skse64_loader.exe')


def test_stock_layout_does_not_copy_personal_game_mods(setup):
    make_stock(setup)
    (setup['base'] / 'personal.dll').write_bytes(b'must stay personal')
    package = setup['package'].with_name('stock.skyrimsync')
    export_setup(base=setup['base'], installed=setup['installed'], artifact=setup['artifact'], output=package,
                 name='Stock', profile='Coop', layout='stock-mo2', standard_root_builder=True)
    result = install(load(package), base=setup['base'], root=setup['root'],
                     reconstructed=setup['installed'], accepted=True)
    assert not (result / 'game/personal.dll').exists()
    assert (result / 'game/skse64_loader.exe').is_file()


@pytest.mark.skipif(__import__('os').name == 'nt', reason='Case-sensitive source directories')
def test_cross_mod_directory_casing_is_materialized_as_one_directory(setup):
    directory = setup['installed'] / 'mods/Low/Textures'
    (setup['installed'] / 'mods/Low/textures').rename(directory)
    (directory / 'other.dds').write_bytes(b'other texture')
    package = setup['package'].with_name('casing.skyrimsync')
    export_setup(base=setup['base'], installed=setup['installed'], artifact=setup['artifact'], output=package,
                 name='Casing', profile='Coop')
    result = install(load(package), base=setup['base'], root=setup['root'],
                     reconstructed=setup['installed'], accepted=True)
    assert not verify(load(package), result)
    assert len([p for p in (result / 'game/Data').iterdir() if p.is_dir() and p.name.casefold() == 'textures']) == 1


def test_code_workflow_checks_host_identity_and_reuses_existing_install(setup):
    make_stock(setup)
    descriptor = {**codes.SME, 'profile': 'Coop', 'name': 'Synthetic code fixture',
                  'sha256': hashlib.sha256(setup['artifact'].read_bytes()).hexdigest(),
                  'size': setup['artifact'].stat().st_size}
    discovered = {'base': setup['base'], 'steam': setup['base'], 'proton': None, 'runtime': None}
    events = []
    package = sync_code(codes.encode(descriptor), setup['root'], True, events.append, discovered=discovered,
                        artifact_override=setup['artifact'], reconstructed_override=setup['installed'])
    recipe = load(package)
    shared = events[-1]['code']
    assert codes.decode(shared)['setup_id'] == recipe.setup_id
    assert not verify(recipe, setup['root'] / 'installs' / recipe.setup_id)
    events.clear()
    sync_code(shared, setup['root'], True, events.append, discovered=discovered)
    assert events[-1]['reused']
    mismatch = {**descriptor, 'setup_id': 'b' * 64}
    with pytest.raises(SyncError, match='host target'):
        sync_code(codes.encode(mismatch), setup['root'], True, discovered=discovered)


def test_code_workflow_materializes_owned_reconstruction_with_lowercase_inis(setup):
    make_stock(setup)
    profile = setup['installed'] / 'profiles/Coop'
    (profile / 'Skyrim.ini').rename(profile / 'skyrim.ini')
    (profile / 'initweaks.ini').write_text('[Archive]\nbInvalidateOlderFiles=1\n')
    data = {**codes.SME, 'profile': 'Coop', 'sha256': hashlib.sha256(setup['artifact'].read_bytes()).hexdigest(),
            'size': setup['artifact'].stat().st_size}
    owned = setup['root'] / 'code-jobs' / codes.job_id(data) / 'reconstructed'
    shutil.copytree(setup['installed'], owned)
    package = sync_code(codes.encode(data), setup['root'], True, discovered={'base': setup['base']},
                        artifact_override=setup['artifact'], reconstructed_override=owned)
    recipe = load(package)
    installed = setup['root'] / 'installs' / recipe.setup_id
    assert 'bInvalidateOlderFiles=1' in (installed / 'config/documents/Skyrim.ini').read_text()
    assert not verify(recipe, installed)
    assert (profile / 'skyrim.ini').read_text() == '[General]\nsLanguage=ENGLISH\n'


def test_sharing_checks_final_bytes_and_reuse_needs_no_steam_discovery(setup, monkeypatch, capsys):
    from skyrim_mod_sync.cli import main
    make_stock(setup)
    descriptor = {**codes.SME, 'profile': 'Coop',
                  'sha256': hashlib.sha256(setup['artifact'].read_bytes()).hexdigest(),
                  'size': setup['artifact'].stat().st_size}
    code = codes.encode(descriptor)
    package = sync_code(code, setup['root'], True, discovered={'base': setup['base']},
                        artifact_override=setup['artifact'], reconstructed_override=setup['installed'])
    recipe = load(package)
    monkeypatch.setattr('skyrim_mod_sync.workflow.discover', lambda: pytest.fail('reuse must work offline'))
    sync_code(code, setup['root'], True)
    assert main(['share', code, '--root', str(setup['root']), '--raw']) == 0
    shared = codes.decode(capsys.readouterr().out.strip())
    assert shared['setup_id'] == recipe.setup_id
    (setup['root'] / 'installs' / recipe.setup_id / 'game/SkyrimSE.exe').write_bytes(b'changed')
    assert main(['share', code, '--root', str(setup['root']), '--raw']) == 2
    assert 'SMS1-' not in capsys.readouterr().out
