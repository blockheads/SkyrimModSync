"""Explicit runtime outputs and deterministic defaults for supported plugins."""
from pathlib import Path

from .paths import SyncError, sha256, under

OAR_DLL_SHA256 = 'a908eca3a068c3ea67edc3495e8baa80038c3db345b7e1a051c80b9ad093a1b7'
OAR_SOURCE = '.skyrimsync-generated/runtime/oar-2.3.6.ini'
# The exact file written by OAR 2.3.6 on its first run. Declare it before Play so
# subsequent settings changes remain ordinary fingerprint mismatches.
OAR_DEFAULT = ('\ufeff' + '''[General]
uAnimationLimit = 32767
uHavokHeapSize = 1073741824
bAsyncParsing = true
bLoadDefaultBehaviorsInMainMenu = true


[Filtering]
bFilterOutDuplicateAnimations = true


[UI]
bEnableUI = true
bShowWelcomeBanner = true
uToggleUIKey = 24
uToggleUIKeyCtrl = 0
uToggleUIKeyShift = 1
uToggleUIKeyAlt = 0
fUIScale = 1.000000
bEnableAnimationQueueProgressBar = true
fAnimationQueueLingerTime = 5.000000
bEnableAnimationLog = false
fAnimationLogWidth = 650.000000
uAnimationLogSize = 10
uAnimationActivateLogMode = 2
uAnimationEchoLogMode = 0
uAnimationLoopLogMode = 0
bAnimationLogOnlyActiveGraph = true
bAnimationLogWriteToTextLog = false
bEnableAnimationEventLog = false
bAnimationLogOrderDescending = true
fAnimationEventLogWidth = 500.000000
fAnimationEventLogHeight = 600.000000
bAnimationEventLogWriteToTextLog = false
fAnimationLogsOffsetX = 0.000000
fAnimationLogsOffsetY = 30.000000


[Workarounds]
bLegacyKeepRandomResultsByDefault = true


[Experimental]
bDisablePreloading = false
bIncreaseAnimationLimit = false


[Debug]
bEnableDebugDraws = false
'''.replace('\n', '\r\n')).encode('utf-8')


def generate_runtime_file(directory: Path, source: str):
    if source != OAR_SOURCE:
        raise SyncError('unsupported_default', 'Unknown generated plugin configuration.')
    target = under(directory, source)
    target.parent.mkdir(parents=True, exist_ok=True)
    if target.is_symlink():
        raise SyncError('unsafe_source', 'Generated plugin settings cannot be symbolic links.')
    target.write_bytes(OAR_DEFAULT)
    return target


def add_runtime_defaults(winners, installed):
    dll = winners.get('game/data/skse/plugins/openanimationreplacer.dll')
    key = 'game/data/skse/plugins/openanimationreplacer.ini'
    if dll and key not in winners:
        if sha256(dll[1]) != OAR_DLL_SHA256:
            raise SyncError('unsupported_default', 'Supply an explicit OpenAnimationReplacer.ini for this plugin version.')
        target = str(Path(dll[0]).with_suffix('.ini'))
        path = generate_runtime_file(installed, OAR_SOURCE)
        winners[key] = (target, path, {'kind': 'wabbajack', 'path': OAR_SOURCE})


def runtime_logs(expected):
    names = {name.casefold() for name in expected}
    outputs = set()
    if 'game/d3dx9_42.dll' in names:
        outputs.add('game/d3dx9_42.log')
    for plugin in ('ActorLimitFix', 'BarterLimitFix', 'BugFixesSSE', 'ScrambledBugs'):
        path = 'game/data/skse/plugins/' + plugin.casefold()
        if path + '.dll' in names:
            outputs.add(path + '.log')
    return outputs
