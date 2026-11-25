import hashlib
import os
import shutil
import pytest

os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

from PySide6.QtCore import QCoreApplication, QEvent, QEventLoop, QSettings, QTimer
from PySide6.QtWidgets import QApplication

from skyrim_mod_sync import codes
from skyrim_mod_sync.gui import Window
from skyrim_mod_sync.installer import install, verify

# Keep Qt alive until all window and deferred process objects have been released.
APP = QApplication.instance() or QApplication([])


@pytest.fixture(autouse=True)
def release_windows():
    yield
    for widget in APP.topLevelWidgets():
        widget.close()
        widget.deleteLater()
    QCoreApplication.sendPostedEvents(None, QEvent.Type.DeferredDelete)


def cached_setup(setup):
    destination = install(setup['recipe'], base=setup['base'], root=setup['root'],
                          reconstructed=setup['installed'], accepted=True)
    descriptor = {**codes.SME, 'profile': 'Coop', 'name': 'Synthetic cached setup',
                  'sha256': hashlib.sha256(setup['artifact'].read_bytes()).hexdigest(),
                  'size': setup['artifact'].stat().st_size, 'setup_id': setup['recipe'].setup_id}
    job = setup['root'] / 'code-jobs' / codes.job_id(descriptor)
    job.mkdir(parents=True)
    shutil.copyfile(setup['package'], job / 'setup.skyrimsync')
    return codes.encode(descriptor), destination


def window_at(tmp_path):
    app = APP
    QSettings.setPath(QSettings.Format.NativeFormat, QSettings.Scope.UserScope, str(tmp_path / 'qt'))
    return app, Window()


def wait_worker(window):
    loop = QEventLoop()
    window.process.finished.connect(loop.quit)
    QTimer.singleShot(15000, loop.quit)
    loop.exec()
    assert window.process is None


def test_native_sync_button_drives_public_cli_worker_without_game_or_network(setup, tmp_path):
    code, destination = cached_setup(setup)
    app, window = window_at(tmp_path)
    window.root.setText(str(setup['root']))
    window.code_input.setText(code)
    window.sync_button.click()
    assert window.process is not None
    assert window.worker_arguments[0] == 'sync'
    assert not window.launch_button.isEnabled()
    wait_worker(window)
    assert window.status.text() == 'Setup verified. Ready to play.'
    assert window.ready
    assert window.copy_code.isEnabled()
    assert window.launch_button.isEnabled() == (os.name != 'nt')
    assert codes.decode(window.active_code)['setup_id'] == setup['recipe'].setup_id
    assert not verify(setup['recipe'], destination)
    window.close()
    reopened = Window()
    assert reopened.ready
    assert 'Play will verify' in reopened.status.text()
    reopened.close()


def test_native_sync_refuses_changed_bytes_and_keeps_play_disabled(setup, tmp_path):
    code, destination = cached_setup(setup)
    (destination / 'game/SkyrimSE.exe').write_bytes(b'changed')
    app, window = window_at(tmp_path)
    window.root.setText(str(setup['root']))
    window.code_input.setText(code)
    window.sync_button.click()
    wait_worker(window)
    assert not window.ready
    assert not window.launch_button.isEnabled()
    assert not window.copy_code.isEnabled()
    assert (destination / 'game/SkyrimSE.exe').read_bytes() == b'changed'
    window.close()


def test_editing_code_or_storage_invalidates_ready_target(setup, tmp_path):
    code, _ = cached_setup(setup)
    app, window = window_at(tmp_path)
    window.root.setText(str(setup['root']))
    window.code_input.setText(code)
    window.show_event({'state': 'files_verified', 'code': code})
    window.update_controls()
    assert window.ready
    window.code_input.textEdited.emit('another target')
    assert not window.ready
    assert window.active_code is None
    assert not window.launch_button.isEnabled()
    window.show_event({'state': 'files_verified', 'code': code})
    window.root.setText(str(tmp_path / 'different-storage'))
    assert not window.ready
    window.close()


def test_recipe_byte_progress_fits_qt_integer_range(tmp_path):
    app, window = window_at(tmp_path)
    window.show_event({'state': 'downloading_recipe', 'completed': 4 * 1024**3, 'total': 8 * 1024**3})
    assert window.progress.value() == 50
    window.close()
