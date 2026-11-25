"""Materialize portable game INIs from a selected reconstructed profile."""
from __future__ import annotations

import configparser
import io
from pathlib import Path

from .paths import SyncError, locate, under


def profile_inis(installed: Path, profile: str):
    selected = under(installed, 'profiles/' + profile)
    tweaks = configparser.ConfigParser(interpolation=None, strict=False)
    tweaks.optionxform = str
    file = selected / 'initweaks.ini'
    if file.is_file():
        tweaks.read_string(file.read_text(encoding='utf-8-sig'))
    for section in tweaks.sections():
        if section.casefold() != 'archive' or any(k.casefold() not in {'binvalidateolderfiles', 'sresourcedatadirsfinal'} for k in tweaks[section]):
            raise SyncError('unsupported_layout', 'This profile has unsupported INI tweaks; the supported adapter handles archive invalidation.')
    result = {}
    for name in ('Skyrim.ini', 'SkyrimPrefs.ini', 'SkyrimCustom.ini'):
        try:
            source = locate(selected, name)
        except SyncError as exc:
            if exc.code == 'missing_file':
                continue
            raise
        raw = source.read_bytes()
        if name == 'Skyrim.ini' and tweaks.sections():
            ini = configparser.ConfigParser(interpolation=None, strict=False)
            ini.optionxform = str
            ini.read_string(raw.decode('utf-8-sig'))
            for section in tweaks.sections():
                actual_section = next((s for s in ini.sections() if s.casefold() == section.casefold()), section)
                if not ini.has_section(actual_section):
                    ini.add_section(actual_section)
                for key, value in tweaks[section].items():
                    actual_key = next((k for k in ini[actual_section] if k.casefold() == key.casefold()), key)
                    ini[actual_section][actual_key] = value
            output = io.StringIO()
            ini.write(output, space_around_delimiters=False)
            raw = output.getvalue().encode('utf-8')
        destination = under(installed, '.skyrimsync-generated/profiles/' + profile + '/' + name)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(raw)
        result[name] = destination
    return result
