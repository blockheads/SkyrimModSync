import hashlib

import pytest

from skyrim_mod_sync.exporter import export_setup
from skyrim_mod_sync.installer import install, verify
from skyrim_mod_sync.paths import SyncError
from skyrim_mod_sync.recipe import load
from skyrim_mod_sync.runtime_files import OAR_SOURCE


def export_runtime(setup, output):
    export_setup(base=setup['base'], installed=setup['installed'], artifact=setup['artifact'],
                 output=output, name='Runtime fixture', profile='Coop', runtime_only=True)
    return load(output)


def test_known_logs_do_not_hide_modified_settings_or_extra_plugins(setup):
    plugins = setup['installed'] / 'mods/High/SKSE/Plugins'
    plugins.mkdir(parents=True)
    (plugins / 'ActorLimitFix.dll').write_bytes(b'synthetic plugin')
    (plugins / 'ActorLimitFix.ini').write_bytes(b'expected settings')
    recipe = export_runtime(setup, setup['package'].with_name('runtime.skyrimsync'))
    destination = install(recipe, base=setup['base'], root=setup['root'],
                          reconstructed=setup['installed'], accepted=True)
    target = destination / 'game/Data/SKSE/Plugins'
    (target / 'ActorLimitFix.log').write_bytes(b'generated diagnostic output')
    assert not verify(recipe, destination)
    (target / 'ActorLimitFix.ini').write_bytes(b'modified settings')
    (target / 'unregistered.dll').write_bytes(b'extra plugin')
    (target / 'unregistered.log').write_bytes(b'unrecognized output')
    differences = {(d['kind'], d['path']) for d in verify(recipe, destination)}
    assert ('changed', 'game/Data/SKSE/Plugins/ActorLimitFix.ini') in differences
    assert ('extra', 'game/Data/SKSE/Plugins/unregistered.dll') in differences
    assert ('extra', 'game/Data/SKSE/Plugins/unregistered.log') in differences


def test_missing_supported_plugin_default_is_rebuilt_and_verified(setup, monkeypatch):
    plugins = setup['installed'] / 'mods/High/SKSE/Plugins'
    plugins.mkdir(parents=True)
    dll = b'synthetic supported OAR plugin'
    (plugins / 'OpenAnimationReplacer.dll').write_bytes(dll)
    monkeypatch.setattr('skyrim_mod_sync.runtime_files.OAR_DLL_SHA256', hashlib.sha256(dll).hexdigest())
    recipe = export_runtime(setup, setup['package'].with_name('defaults.skyrimsync'))
    (setup['installed'] / OAR_SOURCE).unlink()
    destination = install(recipe, base=setup['base'], root=setup['root'],
                          reconstructed=setup['installed'], accepted=True)
    assert not verify(recipe, destination)
    target = destination / 'game/Data/SKSE/Plugins/OpenAnimationReplacer.ini'
    target.write_bytes(b'changed')
    assert {'kind': 'changed', 'path': 'game/Data/SKSE/Plugins/OpenAnimationReplacer.ini'} in verify(recipe, destination)


def test_unknown_plugin_version_needs_explicit_configuration(setup):
    plugins = setup['installed'] / 'mods/High/SKSE/Plugins'
    plugins.mkdir(parents=True)
    (plugins / 'OpenAnimationReplacer.dll').write_bytes(b'unknown plugin version')
    with pytest.raises(SyncError, match='explicit OpenAnimationReplacer.ini'):
        export_runtime(setup, setup['package'].with_name('unknown.skyrimsync'))
