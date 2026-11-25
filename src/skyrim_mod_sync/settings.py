"""Local path choices; credentials are stored separately and never serialized here."""
from __future__ import annotations

import json
import os
from pathlib import Path

from .installer import atomic_json
from .paths import SyncError

FIELDS = {'base', 'coop', 'downloads', 'game', 'documents', 'local', 'prefix', 'steam'}


def env_directory(name, default):
    value = os.environ.get(name, '')
    path = Path(value) if value else None
    return path if path and path.is_absolute() else default


def config_directory():
    if os.name == 'nt':
        return env_directory('LOCALAPPDATA', Path.home() / 'AppData/Local') / 'SkyrimModSync'
    return env_directory('XDG_CONFIG_HOME', Path.home() / '.config') / 'skyrim-mod-sync'


def default_storage():
    if os.name == 'nt':
        return env_directory('LOCALAPPDATA', Path.home() / 'AppData/Local') / 'SkyrimModSync'
    return env_directory('XDG_DATA_HOME', Path.home() / '.local/share') / 'skyrim-mod-sync'


def load_settings():
    path = config_directory() / 'config.json'
    if not path.exists():
        return {}
    try:
        if path.is_symlink() or path.stat().st_size > 65536:
            raise ValueError('Invalid settings file')
        data = json.loads(path.read_text())
        if not isinstance(data, dict) or set(data) - FIELDS:
            raise ValueError('Unknown settings')
        if any(not isinstance(v, str) or not Path(v).is_absolute() for v in data.values()):
            raise ValueError('Paths must be absolute')
        return data
    except (OSError, ValueError):
        raise SyncError('invalid_configuration', f'Fix or remove the local path configuration: {path}') from None


def save_settings(values):
    data = load_settings()
    if set(values) - FIELDS:
        raise SyncError('invalid_configuration', 'Only local path choices belong in configuration.')
    data.update({k: str(Path(v).expanduser().resolve()) for k, v in values.items() if v is not None})
    path = config_directory() / 'config.json'
    if path.is_symlink():
        raise SyncError('invalid_configuration', 'The local configuration must not be a symlink.')
    atomic_json(path, data)
    if os.name != 'nt':
        path.chmod(0o600)
    return data


def resolve(names, supplied, *, interactive=False):
    saved = load_settings()
    result = {}
    for name in names:
        value = supplied.get(name) or saved.get(name)
        if not value and interactive:
            value = input(f'{name.capitalize()} folder: ').strip()
        if not value:
            raise SyncError('configuration_required', f'Supply --{name} PATH, or run configure to save the {name} folder.')
        result[name] = Path(value).expanduser().resolve()
    return result
