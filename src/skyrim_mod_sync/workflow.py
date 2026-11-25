"""Pinned code -> acquisition -> ordinary managed game -> verified Play target."""
from __future__ import annotations

import json
import shutil
from pathlib import Path
from zipfile import ZipFile

from . import codes
from .auth import load_key, validate_key
from .bootstrap import ensure_engine
from .discovery import discover
from .download import download_list, wj_hash
from .exporter import export_setup
from .installer import _separate, atomic_json, install, job_lock, run_engine, verify
from .isolation import engine_context
from .paths import SyncError, locate, sha256, under
from .recipe import load


def check_base(artifact, base, check_cancel=lambda: None):
    with ZipFile(artifact) as archive:
        data = json.loads(archive.read('modlist'))
    problems = []
    for item in data['Archives']:
        state = item.get('State', {})
        if 'GameFileSource' not in state.get('$type', ''):
            continue
        check_cancel()
        if state.get('Game') != 'SkyrimSpecialEdition':
            problems.append({'path': state.get('GameFile', ''), 'reason': 'unsupported_game_source'})
            continue
        relative = state['GameFile'].replace('\\', '/')
        try:
            source = locate(base, relative)
            if source.stat().st_size != item['Size'] or wj_hash(source, check_cancel) != item['Hash']:
                problems.append({'path': relative, 'reason': 'base_mismatch'})
        except SyncError as exc:
            if exc.code == 'cancelled':
                raise
            problems.append({'path': relative, 'reason': exc.code})
    return problems


def verify_reconstruction(artifact, directory, emit=lambda _: None, check_cancel=lambda: None):
    with ZipFile(artifact) as archive:
        data = json.loads(archive.read('modlist'))
    directives = data['Directives']
    differences = []
    for index, directive in enumerate(directives, 1):
        check_cancel()
        kind = directive['$type']
        if kind == 'RemappedInlineFile':
            continue  # Explicitly machine-local paths are intentionally nondeterministic.
        if kind not in {'InlineFile', 'FromArchive', 'PatchedFromArchive'}:
            raise SyncError('unsupported_recipe', f'Unsupported reconstruction instruction: {kind}')
        relative = directive['To'].replace('\\', '/')
        path = under(directory, relative)
        if not path.is_file() or path.is_symlink():
            differences.append({'path': relative, 'reason': 'missing'})
        elif kind == 'InlineFile' and path.name.casefold() == 'modlist.txt':
            # SME's directive size/hash is stale for this file. Compare against the
            # exact embedded payload, covered by the pinned whole-recipe SHA-256.
            with ZipFile(artifact) as archive:
                entry = archive.getinfo(directive['SourceDataID'])
                if entry.file_size > 64 * 1024**2 or path.read_bytes() != archive.read(entry):
                    differences.append({'path': relative, 'reason': 'content_mismatch'})
        elif path.stat().st_size != directive['Size'] or wj_hash(path, check_cancel) != directive['Hash']:
            differences.append({'path': relative, 'reason': 'content_mismatch'})
        if index == 1 or index % 100 == 0 or index == len(directives):
            emit({'state': 'reconstruction_verifying', 'completed': index, 'total': len(directives)})
    return differences


def sync_code(code: str, root: Path, accepted=False, emit=lambda _: None, *,
              discovered=None, artifact_override=None, reconstructed_override=None):
    if not accepted:
        raise SyncError('consent_required', 'Review the setup code and approve Sync first.')
    descriptor = codes.decode(code)
    root = root.resolve()
    identity = codes.job_id(descriptor)
    if reconstructed_override and reconstructed_override.resolve() != (root / 'code-jobs' / identity / 'reconstructed').resolve():
        _separate(root, reconstructed_override)
    job = root / 'code-jobs' / identity
    root.mkdir(parents=True, exist_ok=True)
    with job_lock(root / 'locks' / ('code-' + identity + '.lock')):
        job.mkdir(parents=True, exist_ok=True)
        cancel = job / 'cancel'
        cancel.unlink(missing_ok=True)
        def check_cancel():
            if cancel.exists():
                raise SyncError('cancelled', 'Sync cancelled; cached downloads are retained.')
        def event(state, **details):
            data = {'state': state, **details}
            atomic_json(job / 'status.json', data)
            with (job / 'events.jsonl').open('a') as out:
                out.write(json.dumps(data) + '\n')
            emit(data)
        def forward(data):
            if data['state'] == 'exporting' and data.get('completed') not in {1, data.get('total')} and data.get('completed', 0) % 25:
                return
            event(data['state'], **{k: v for k, v in data.items() if k != 'state'})
        try:
            saved = job / 'setup.skyrimsync'
            if saved.is_file():
                recipe = load(saved)
                destination = root / 'installs' / recipe.setup_id
                if descriptor['setup_id'] and recipe.setup_id != descriptor['setup_id']:
                    raise SyncError('host_mismatch', 'This local installation differs from the shared host target.')
                if destination.exists():
                    differences = verify(recipe, destination, check_cancel)
                    if differences:
                        raise SyncError('installed_setup_changed',
                                        'The existing managed setup changed; inspect its differences before syncing again.')
                    shared = {**descriptor, 'setup_id': recipe.setup_id}
                    event('files_verified', setup_id=recipe.setup_id, package=str(saved), code=codes.encode(shared), reused=True)
                    return saved
            discovered = discovered or discover()
            base = discovered['base']
            _separate(root, base)
            event('preparing_recipe', name=descriptor['name'])
            artifacts = root / 'recipes'
            artifacts.mkdir(exist_ok=True)
            artifact = artifacts / (descriptor['sha256'] + '.wabbajack')
            if not artifact.is_file() or artifact.stat().st_size != descriptor['size'] or sha256(artifact, check_cancel) != descriptor['sha256']:
                artifact.unlink(missing_ok=True)
                if artifact_override:
                    shutil.copyfile(artifact_override, artifact)
                else:
                    download_list(descriptor['url'], artifact, root / 'recipe-downloads', forward, check_cancel)
            if artifact.stat().st_size != descriptor['size'] or sha256(artifact, check_cancel) != descriptor['sha256']:
                artifact.unlink(missing_ok=True)
                raise SyncError('artifact_mismatch', 'The downloaded recipe differs from this pinned setup code.')
            event('checking_base')
            problems = check_base(artifact, base, check_cancel)
            if problems:
                event('base_requirements', differences=problems)
                raise SyncError('base_mismatch', 'Required Skyrim files differ: ' + ', '.join(p['path'] for p in problems[:8]))
            if reconstructed_override is None:
                with ZipFile(artifact) as archive:
                    raw = json.loads(archive.read('modlist'))
                # Reconstruction, final copied game and extraction working space coexist.
                required = 2 * sum(d.get('Size', 0) for d in raw['Directives']) + 2 * sum(
                    a.get('Size', 0) for a in raw['Archives'] if 'GameFileSource' not in a.get('State', {}).get('$type', ''))
                if shutil.disk_usage(root).free < required:
                    raise SyncError('disk_full', f'Choose managed storage with at least {required / 1024**3:.1f} GiB free for this setup.')
            reconstructed = reconstructed_override
            if reconstructed is None:
                key = load_key(required=True)
                account = validate_key(key)
                if not account['premium']:
                    raise SyncError('manual_downloads_required', 'This automated path needs Nexus Premium; manual-download orchestration is not implemented yet.')
                engine = ensure_engine(root, forward, check_cancel)
                reconstructed = job / 'reconstructed'
                reconstructed.mkdir(exist_ok=True)
                downloads = root / 'downloads'
                downloads.mkdir(exist_ok=True)
                prefix, environment = engine_context(root, job, discovered['steam'], discovered.get('proton'), key,
                                                      base=base, downloads=downloads)
                event('reconstructing', log=str(job / 'engine.log'))
                run_engine(engine, artifact, reconstructed, downloads, job / 'engine.log', check_cancel,
                           engine_kind='jackify', base=base, command_prefix=prefix, environment=environment, emit=forward)
            event('reconstruction_verifying')
            differences = verify_reconstruction(artifact, reconstructed, forward, check_cancel)
            if differences:
                event('reconstruction_differences', differences=differences[:100])
                raise SyncError('reconstruction_mismatch', f'{len(differences)} reconstructed files differ from the pinned recipe.')
            event('exporting')
            from .profiles import profile_inis
            ini_files = profile_inis(reconstructed, descriptor['profile'])
            temporary_package = job / 'candidate.skyrimsync'
            temporary_package.unlink(missing_ok=True)
            export_setup(base=base, installed=reconstructed, artifact=artifact, output=temporary_package,
                         name=descriptor['name'], profile=descriptor['profile'], layout='stock-mo2',
                         standard_root_builder=True, recipe_url=descriptor['url'], emit=forward, runtime_only=True,
                         profile_ini_overrides=ini_files)
            recipe = load(temporary_package)
            if descriptor['setup_id'] and recipe.setup_id != descriptor['setup_id']:
                raise SyncError('host_mismatch', 'The reconstructed setup differs from the host fingerprint; it will not be published.')
            event('staging')
            install(recipe, base=base, root=root, reconstructed=reconstructed, accepted=True,
                    emit=lambda data: forward(data) if data['state'] != 'files_verified' else None,
                    managed_source=reconstructed.resolve().is_relative_to(root / 'code-jobs'))
            temporary_package.replace(saved)
            shared = {**descriptor, 'setup_id': recipe.setup_id}
            event('files_verified', setup_id=recipe.setup_id, package=str(saved), code=codes.encode(shared), reused=False)
            return saved
        except BaseException as exc:
            reason = exc.code if isinstance(exc, SyncError) else 'cancelled' if isinstance(exc, KeyboardInterrupt) else 'sync_failed'
            event('cancelled' if reason == 'cancelled' else 'failed', reason=reason, message=str(exc))
            raise


def synced_recipe(code, root):
    descriptor = codes.decode(code)
    saved = root.resolve() / 'code-jobs' / codes.job_id(descriptor) / 'setup.skyrimsync'
    if not saved.is_file():
        raise SyncError('setup_missing', 'Sync this setup before pressing Play.')
    recipe = load(saved)
    if descriptor['setup_id'] and descriptor['setup_id'] != recipe.setup_id:
        raise SyncError('host_mismatch', 'The installed target does not match the shared setup code.')
    return descriptor, recipe


def share_code(code, root):
    descriptor, recipe = synced_recipe(code, root)
    directory = root.resolve() / 'installs' / recipe.setup_id
    if not directory.is_dir() or verify(recipe, directory):
        raise SyncError('setup_not_verified', 'The managed installation must pass verification before sharing.')
    return codes.encode({**descriptor, 'setup_id': recipe.setup_id})


def play_code(code, root, emit=lambda _: None):
    from .launch import launch
    descriptor, recipe = synced_recipe(code, root)
    discovered = discover()
    if not discovered['proton'] or not discovered['runtime']:
        raise SyncError('runtime_missing', 'Install Proton 10 and Steam Linux Runtime sniper through Steam before Play.')
    emit({'state': 'verifying'})
    return launch(recipe, root=root, proton=discovered['proton'], steam=discovered['steam'], runtime=discovered['runtime'], emit=emit)
