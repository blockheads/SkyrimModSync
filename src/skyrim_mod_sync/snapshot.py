"""Fresh, portable comparisons of physically deployed Skyrim files and profiles."""
from __future__ import annotations

import hashlib
import json
import re
import struct
from pathlib import Path

from .paths import SyncError, inventory, locate, relative_path, sha256
from .recipe import MAX_FILES, MAX_MANIFEST, _identity, _unique_object, canonical
from .runtime_files import runtime_logs

DOCUMENT_FILES = ('Skyrim.ini', 'SkyrimPrefs.ini', 'SkyrimCustom.ini')
LOCAL_FILES = ('plugins.txt', 'loadorder.txt')
BASE_PLUGINS = ('Skyrim.esm', 'Update.esm', 'Dawnguard.esm', 'HearthFires.esm', 'Dragonborn.esm')


def binary_version(path):
    """Read PE resources without running Windows code. Missing metadata is explicit."""
    if path is None or path.stat().st_size > 128 * 1024**2:
        return None
    import pefile
    try:
        with pefile.PE(str(path), fast_load=True) as pe:
            pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY['IMAGE_DIRECTORY_ENTRY_RESOURCE']])
            if not getattr(pe, 'VS_FIXEDFILEINFO', None):
                return None
            info = pe.VS_FIXEDFILEINFO[0]
            return '.'.join(str(n) for n in (info.FileVersionMS >> 16, info.FileVersionMS & 0xffff,
                                             info.FileVersionLS >> 16, info.FileVersionLS & 0xffff))
    except (pefile.PEFormatError, OSError, ValueError, IndexError):
        return None


def optional(root, name):
    if root is None or not root.is_dir():
        return None
    try:
        return locate(root, name)
    except SyncError as exc:
        if exc.code == 'missing_file':
            return None
        raise


def lines(path):
    if path is None:
        return []
    if path.stat().st_size > 4 * 1024**2:
        raise SyncError('invalid_configuration', f'Configuration is too large: {path.name}')
    return [line.strip() for line in path.read_text(encoding='utf-8-sig').splitlines()
            if line.strip() and not line.lstrip().startswith(('#', ';'))]


def plugins(game, local):
    activation = optional(local, 'plugins.txt')
    entries = lines(activation)
    starred = any(line.startswith('*') for line in entries)
    active = [line.lstrip('*') for line in entries if not starred or line.startswith('*')]
    forced = [name for name in BASE_PLUGINS if optional(game, 'Data/' + name)]
    for name in lines(optional(game, 'Skyrim.ccc')) + ['_ResourcePack.esl']:
        relative_path(name)
        if '/' in name:
            raise SyncError('invalid_configuration', 'Creation entries must be plugin filenames.')
        if optional(game, 'Data/' + name) and name.casefold() not in {n.casefold() for n in forced}:
            forced.append(name)
    if len({name.casefold() for name in active}) != len(active):
        raise SyncError('invalid_configuration', 'Duplicate plugin activation entries.')
    requested = dict((name.casefold(), name) for name in forced + active)
    for name in requested.values():
        relative_path(name)
        if '/' in name or Path(name).suffix.casefold() not in {'.esm', '.esp', '.esl'}:
            raise SyncError('invalid_configuration', f'Invalid plugin filename: {name}')
    ordered = []
    for name in lines(optional(local, 'loadorder.txt')):
        if name.casefold() in requested:
            ordered.append(requested.pop(name.casefold()))
    ordered += list(requested.values())
    result, warnings = [], []
    for name in ordered:
        file = optional(game, 'Data/' + name)
        light = None
        if file:
            with file.open('rb') as stream:
                header = stream.read(12)
            if len(header) == 12 and header[:4] == b'TES4':
                light = bool(struct.unpack_from('<I', header, 8)[0] & 0x200)
            else:
                warnings.append({'reason': 'invalid_plugin_header', 'path': 'game/Data/' + name})
        else:
            warnings.append({'reason': 'missing_active_plugin', 'path': 'game/Data/' + name})
        result.append({'name': name, 'light': light})
    if activation is None:
        warnings.append({'reason': 'activation_unavailable', 'path': 'config/local/plugins.txt'})
    return result, warnings


def ignored_game(name, logs):
    lower = name.casefold()
    return (lower in logs or lower.startswith(('.skyrimsync/', 'saves/', 'logs/'))
            or lower.endswith(('.ess', '.skse')))


def capture(game: Path, documents: Path | None, local: Path | None, *, emit=lambda _: None,
            check_cancel=lambda: None):
    game = game.resolve()
    if not game.is_dir():
        raise SyncError('missing_directory', f'Skyrim folder is missing: {game}. Supply --game pointing to the deployed game.')
    documents = documents.resolve() if documents else None
    local = local.resolve() if local else None
    entries = list(inventory(game))
    logs = {p.removeprefix('game/') for p in runtime_logs('game/' + n for n, _ in entries)}
    selected = [('game/' + name, path) for name, path in entries if not ignored_game(name, logs)]
    for root, names, prefix in ((documents, DOCUMENT_FILES, 'config/documents/'),
                                (local, LOCAL_FILES, 'config/local/')):
        for name in names:
            path = optional(root, name)
            if path:
                selected.append((prefix + name, path))
    if len(selected) > MAX_FILES:
        raise SyncError('snapshot_too_large', 'Too many files in the deployed setup.')
    files = []
    for index, (name, path) in enumerate(sorted(selected), 1):
        check_cancel()
        before = path.stat()
        digest = sha256(path, check_cancel)
        after = path.stat()
        if (before.st_size, before.st_mtime_ns, before.st_ino) != (after.st_size, after.st_mtime_ns, after.st_ino):
            raise SyncError('source_changed', f'File changed during capture: {name}. Stop Skyrim and retry.')
        files.append({'path': name, 'size': after.st_size, 'sha256': digest})
        if index == 1 or index % 25 == 0 or index == len(selected):
            emit({'state': 'capturing', 'file': name, 'completed': index, 'total': len(selected)})
    loaded, warnings = plugins(game, local)
    coop = next((f['sha256'] for f in files if f['path'].casefold() == 'game/data/skse/plugins/skyrimtogetherclient.dll'), None)
    result = {'schema': 2, 'kind': 'deployed-skyrim-snapshot', 'policy': 'deployed-exact-v1',
              'files': files, 'versions': {'skyrim': binary_version(optional(game, 'SkyrimSE.exe')),
                                          'skse': binary_version(optional(game, 'skse64_loader.exe')),
                                          'coop_sha256': coop}, 'plugins': loaded, 'warnings': warnings}
    validate(result)
    return result


def validate(data):
    required = {'schema', 'kind', 'policy', 'files', 'versions', 'plugins', 'warnings'}
    if (not isinstance(data, dict) or set(data) != required or type(data['schema']) is not int
            or data['schema'] != 2 or data['kind'] != 'deployed-skyrim-snapshot' or data['policy'] != 'deployed-exact-v1'):
        raise SyncError('invalid_snapshot', 'Unsupported deployed snapshot format or matching policy.')
    if not isinstance(data['files'], list) or len(data['files']) > MAX_FILES:
        raise SyncError('invalid_snapshot', 'Invalid snapshot file list.')
    seen = set()
    for item in data['files']:
        if not isinstance(item, dict) or set(item) != {'path', 'size', 'sha256'}:
            raise SyncError('invalid_snapshot', 'Invalid snapshot file identity.')
        name = relative_path(item['path'])
        if not name.startswith(('game/', 'config/documents/', 'config/local/')):
            raise SyncError('invalid_snapshot', 'File is outside the declared game/configuration roots.')
        _identity(item)
        if name.casefold() in seen:
            raise SyncError('case_collision', 'Duplicate snapshot destination: ' + name)
        seen.add(name.casefold())
    for name in seen:
        if any('/'.join(name.split('/')[:i]) in seen for i in range(1, len(name.split('/')))):
            raise SyncError('invalid_snapshot', 'A snapshot path is both a file and a directory.')
    if not isinstance(data['versions'], dict) or set(data['versions']) != {'skyrim', 'skse', 'coop_sha256'}:
        raise SyncError('invalid_snapshot', 'Invalid binary version information.')
    for name, value in data['versions'].items():
        pattern = '[0-9a-f]{64}' if name == 'coop_sha256' else r'\d+\.\d+\.\d+\.\d+'
        if value is not None and (not isinstance(value, str) or not re.fullmatch(pattern, value)):
            raise SyncError('invalid_snapshot', 'Invalid binary version identity.')
    if not isinstance(data['plugins'], list) or len(data['plugins']) > 65535:
        raise SyncError('invalid_snapshot', 'Invalid plugin sequence.')
    names = set()
    for item in data['plugins']:
        if not isinstance(item, dict) or set(item) != {'name', 'light'} or item['light'] not in (True, False, None):
            raise SyncError('invalid_snapshot', 'Invalid plugin entry.')
        relative_path(item['name'])
        if '/' in item['name'] or item['name'].casefold() in names:
            raise SyncError('invalid_snapshot', 'Duplicate or invalid plugin name.')
        names.add(item['name'].casefold())
    if not isinstance(data['warnings'], list) or len(data['warnings']) > 65536:
        raise SyncError('invalid_snapshot', 'Invalid snapshot warnings.')
    for item in data['warnings']:
        if not isinstance(item, dict) or set(item) != {'reason', 'path'} or not isinstance(item['reason'], str):
            raise SyncError('invalid_snapshot', 'Invalid snapshot diagnostic.')
        relative_path(item['path'])
    return data


def load(path):
    if path.stat().st_size > MAX_MANIFEST:
        raise SyncError('invalid_snapshot', 'Snapshot exceeds the size limit.')
    try:
        return validate(json.loads(path.read_text(encoding='utf-8'), object_pairs_hook=_unique_object))
    except (ValueError, TypeError, KeyError) as exc:
        raise SyncError('invalid_snapshot', 'Cannot read this deployed snapshot.') from exc


def identity(data):
    validate(data)
    semantic = {**data, 'warnings': [],
                'files': sorted(({**f, 'path': f['path'].casefold()} for f in data['files']), key=lambda f: f['path']),
                'plugins': [{**p, 'name': p['name'].casefold()} for p in data['plugins']]}
    return hashlib.sha256(canonical(semantic)).hexdigest()


def compare(expected, actual):
    validate(expected)
    validate(actual)
    differences = []
    for name in ('skyrim', 'skse', 'coop_sha256'):
        if expected['versions'][name] != actual['versions'][name]:
            differences.append({'kind': 'version', 'component': name, 'expected': expected['versions'][name],
                                'actual': actual['versions'][name]})
    want = {f['path'].casefold(): f for f in expected['files']}
    have = {f['path'].casefold(): f for f in actual['files']}
    for key in sorted(want.keys() | have.keys()):
        if key not in have:
            differences.append({'kind': 'missing', 'path': want[key]['path']})
        elif key not in want:
            differences.append({'kind': 'extra', 'path': have[key]['path']})
        elif (want[key]['size'], want[key]['sha256']) != (have[key]['size'], have[key]['sha256']):
            differences.append({'kind': 'changed', 'path': want[key]['path'],
                                'expected': want[key]['sha256'], 'actual': have[key]['sha256']})
    host = {p['name'].casefold(): p for p in expected['plugins']}
    player = {p['name'].casefold(): p for p in actual['plugins']}
    for p in expected['plugins']:
        theirs = player.get(p['name'].casefold())
        if theirs is None:
            differences.append({'kind': 'plugin_missing', 'plugin': p['name']})
        elif theirs['light'] != p['light']:
            differences.append({'kind': 'plugin_light', 'plugin': p['name'], 'expected': p['light'], 'actual': theirs['light']})
    for p in actual['plugins']:
        if p['name'].casefold() not in host:
            differences.append({'kind': 'plugin_extra', 'plugin': p['name']})
    shared_host = [p['name'] for p in expected['plugins'] if p['name'].casefold() in player]
    shared_player = [p['name'] for p in actual['plugins'] if p['name'].casefold() in host]
    for index, (a, b) in enumerate(zip(shared_host, shared_player)):
        if a.casefold() != b.casefold():
            differences.append({'kind': 'plugin_order', 'expected': a, 'actual': b,
                                'after': shared_host[index - 1] if index else ''})
            break
    return {'state': 'files_match' if not differences else 'different', 'files_match': not differences,
            'runtime_verified': False, 'expected_id': identity(expected), 'actual_id': identity(actual),
            'differences': differences, 'warnings': actual['warnings']}


def capture_install(directory, **kwargs):
    return capture(directory / 'game', directory / 'config/documents', directory / 'config/local', **kwargs)
