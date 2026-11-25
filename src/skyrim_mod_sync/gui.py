"""Small optional frontend. Long-running operations use the public CLI."""
from __future__ import annotations

import json
import os
import signal
import sys
from pathlib import Path

from PySide6.QtCore import QProcess, QSettings
from PySide6.QtWidgets import (QApplication, QFileDialog, QFormLayout, QHBoxLayout,
                               QInputDialog, QLabel, QLineEdit, QMainWindow, QMessageBox,
                               QPlainTextEdit, QProgressBar, QPushButton, QVBoxLayout, QWidget)

from .cli import default_storage
from .paths import SyncError


class Window(QMainWindow):
    def __init__(self, package: Path | None = None):
        super().__init__()
        self.setWindowTitle('Skyrim Mod Sync')
        self.resize(580, 340)
        self.settings = QSettings('SkyrimModSync', 'SkyrimModSync')
        self.process = None
        self.buffer = b''
        self.worker_arguments = []
        self.active_code = None
        self.package = None
        self.ready = False
        page = QWidget()
        self.setCentralWidget(page)
        layout = QVBoxLayout(page)
        layout.setContentsMargins(24, 20, 24, 20)
        layout.setSpacing(12)
        title = QLabel('Skyrim Mod Sync')
        title.setStyleSheet('font-size: 24px; font-weight: bold;')
        layout.addWidget(title)
        layout.addWidget(QLabel("Paste your friend's setup code, sync, then play."))
        self.code_input = QLineEdit()
        self.code_input.setPlaceholderText('Setup code — try SME-2.7.1')
        self.code_input.setClearButtonEnabled(True)
        self.code_input.setMinimumHeight(34)
        self.code_input.textEdited.connect(self.target_edited)
        self.code_input.returnPressed.connect(self.start_sync)
        layout.addWidget(self.code_input)
        self.status = QLabel('Ready for a setup code.')
        self.status.setWordWrap(True)
        layout.addWidget(self.status)
        self.progress = QProgressBar()
        self.progress.setRange(0, 100)
        self.progress.setValue(0)
        layout.addWidget(self.progress)
        actions = QHBoxLayout()
        self.sync_button = QPushButton('Sync')
        self.sync_button.clicked.connect(self.start_sync)
        self.launch_button = QPushButton('Play')
        self.launch_button.clicked.connect(self.start_launch)
        self.copy_code = QPushButton('Copy code')
        self.copy_code.clicked.connect(lambda: QApplication.clipboard().setText(self.active_code or ''))
        self.cancel = QPushButton('Cancel')
        self.cancel.clicked.connect(self.cancel_job)
        for button in (self.sync_button, self.launch_button, self.copy_code, self.cancel):
            actions.addWidget(button)
        layout.addLayout(actions)
        links = QHBoxLayout()
        self.options_button = QPushButton('Options')
        self.options_button.setCheckable(True)
        self.options_button.toggled.connect(self.toggle_options)
        self.details_button = QPushButton('Details')
        self.details_button.setCheckable(True)
        self.details_button.toggled.connect(self.toggle_details)
        links.addWidget(self.options_button)
        links.addWidget(self.details_button)
        links.addStretch()
        layout.addLayout(links)
        self.options = QWidget()
        form = QFormLayout(self.options)
        form.setContentsMargins(0, 0, 0, 0)
        storage = QWidget()
        row = QHBoxLayout(storage)
        row.setContentsMargins(0, 0, 0, 0)
        self.root = QLineEdit(str(self.settings.value('root', str(default_storage()))))
        self.root.textChanged.connect(self.target_edited)
        browse = QPushButton('Browse…')
        browse.clicked.connect(self.choose_storage)
        row.addWidget(self.root)
        row.addWidget(browse)
        form.addRow('Storage', storage)
        self.nexus_button = QPushButton('Connect Nexus account…')
        self.nexus_button.clicked.connect(self.connect_nexus)
        form.addRow(self.nexus_button)
        self.pick = QPushButton('Open setup file…')
        self.pick.clicked.connect(self.choose_package)
        form.addRow(self.pick)
        self.options.hide()
        layout.addWidget(self.options)
        self.details = QPlainTextEdit()
        self.details.setReadOnly(True)
        self.details.setMaximumBlockCount(1000)
        self.details.setMinimumHeight(160)
        self.details.hide()
        layout.addWidget(self.details)
        layout.addStretch()
        saved_code = str(self.settings.value('code', ''))
        self.code_input.setText(saved_code)
        if package:
            self.open_package(package)
        elif saved_code:
            self.restore_code(saved_code)
        self.update_controls()

    def toggle_options(self, shown):
        self.options.setVisible(shown)
        self.adjustSize()

    def toggle_details(self, shown):
        self.details.setVisible(shown)
        self.adjustSize()

    def update_controls(self):
        idle = self.process is None
        self.sync_button.setEnabled(idle)
        self.launch_button.setEnabled(idle and self.ready and sys.platform == 'linux')
        self.copy_code.setEnabled(idle and self.ready and self.active_code is not None)
        self.cancel.setEnabled(not idle and self.worker_arguments[0] in {'sync', 'install'})
        self.code_input.setEnabled(idle)
        self.root.setEnabled(idle)
        self.options.setEnabled(idle)

    def target_edited(self, *_):
        self.ready = False
        self.active_code = None
        self.package = None
        self.status.setText('Sync this setup before playing.')
        self.update_controls()

    def restore_code(self, code):
        from .codes import decode, job_id
        from .recipe import load
        try:
            descriptor = decode(code)
            package = Path(self.root.text()).resolve() / 'code-jobs' / job_id(descriptor) / 'setup.skyrimsync'
            recipe = load(package)
            if descriptor['setup_id'] != recipe.setup_id:
                return
            if not (Path(self.root.text()) / 'installs' / recipe.setup_id).is_dir():
                return
            self.active_code = code
            self.ready = True
            self.status.setText(f"{recipe.data['name']}\nSaved setup. Play will verify its files again.")
        except (SyncError, OSError):
            pass

    def choose_storage(self):
        value = QFileDialog.getExistingDirectory(self, 'Choose storage', self.root.text())
        if value:
            self.root.setText(value)
            self.persist()

    def choose_package(self):
        value = QFileDialog.getOpenFileName(self, 'Open setup', '', 'Skyrim setups (*.skyrimsync)')[0]
        if value:
            self.open_package(Path(value))

    def open_package(self, package):
        from .recipe import load
        try:
            recipe = load(package)
            self.target_edited()
            self.package = recipe
            self.code_input.setText(recipe.data['name'])
            size = sum(f['size'] for f in recipe.data['files']) / 1024**3
            self.status.setText(f'{len(recipe.data["files"]):,} files · {size:.1f} GiB. Press Sync to install.')
            self.details.setPlainText(json.dumps(recipe.data, indent=2))
        except (SyncError, OSError) as exc:
            QMessageBox.warning(self, 'Cannot open setup', str(exc))

    def connect_nexus(self):
        key, accepted = QInputDialog.getText(self, 'Connect Nexus',
            'Personal API key from Nexus → Account → API Access:', QLineEdit.EchoMode.Password)
        if accepted and key:
            self.start_worker(['nexus-login', '--stdin'], secret_input=key)

    def persist(self):
        self.settings.setValue('root', self.root.text())
        self.settings.setValue('code', self.active_code or '')

    def start_sync(self):
        if self.process:
            return
        if self.package:
            from .discovery import discover
            try:
                found = discover()
            except SyncError as exc:
                self.status.setText(str(exc))
                return
            arguments = ['install', str(self.package.package), '--base', str(found['base']),
                         '--root', self.root.text(), '--accept']
        else:
            from .codes import decode
            code = self.code_input.text().strip()
            try:
                decode(code)
            except SyncError as exc:
                self.status.setText(str(exc))
                return
            self.active_code = code
            arguments = ['sync', code, '--root', self.root.text(), '--accept']
        self.ready = False
        self.persist()
        self.start_worker(arguments)

    def start_launch(self):
        if not self.ready or self.process:
            return
        if self.active_code:
            self.start_worker(['play', self.active_code, '--root', self.root.text()])
        elif self.package:
            from .discovery import discover
            try:
                found = discover()
                if not found['proton'] or not found['runtime']:
                    raise SyncError('runtime_missing', 'Install Proton 10 and Steam Linux Runtime sniper in Steam.')
            except SyncError as exc:
                self.status.setText(str(exc))
                return
            self.start_worker(['launch', str(self.package.package), '--root', self.root.text(),
                               '--proton', str(found['proton']), '--steam', str(found['steam']),
                               '--runtime', str(found['runtime'])])

    def start_worker(self, arguments, secret_input=None):
        if self.process:
            return
        self.worker_arguments = arguments
        self.buffer = b''
        self.progress.setRange(0, 0)
        self.status.setText('Working…')
        self.details.clear()
        process = QProcess(self)
        self.process = process
        process.readyReadStandardOutput.connect(self.read_output)
        process.finished.connect(self.finished)
        process.errorOccurred.connect(self.worker_error)
        if secret_input is not None:
            def write_secret():
                process.write(secret_input.encode('ascii'))
                process.closeWriteChannel()
            process.started.connect(write_secret)
        worker = ['--cli'] if getattr(sys, 'frozen', False) else ['-m', 'skyrim_mod_sync']
        self.update_controls()
        process.start(sys.executable, worker + arguments)

    def read_output(self):
        if not self.process:
            return
        self.buffer += bytes(self.process.readAllStandardOutput())
        while b'\n' in self.buffer:
            line, self.buffer = self.buffer.split(b'\n', 1)
            try:
                self.show_event(json.loads(line))
            except (ValueError, UnicodeError):
                continue

    def show_event(self, data):
        state = data.get('state', 'working')
        self.details.appendPlainText(json.dumps(data, ensure_ascii=False))
        if data.get('total'):
            self.progress.setRange(0, 100)
            self.progress.setValue(min(100, int(100 * data.get('completed', 0) / data['total'])))
        text = {'checking_base': 'Checking your Skyrim files…',
                'reconstructing': 'Downloading and setting up mods…',
                'preparing_engine': 'Preparing the installer…',
                'downloading_engine': 'Downloading the installer…',
                'engine_ready': 'Installer ready.',
                'preparing_recipe': 'Preparing the modlist…',
                'downloading_recipe': 'Downloading the modlist…',
                'recipe_downloaded': 'Modlist downloaded.',
                'reconstruction_verifying': 'Checking the reconstructed modlist…',
                'staging': 'Building your Skyrim installation…',
                'verifying': 'Verifying files…',
                'exporting': 'Recording the exact setup…',
                'files_verified': 'Setup verified. Ready to play.',
                'initializing_prefix': 'Preparing Skyrim for its first launch…',
                'launch_started': 'Launch started.',
                'failed': data.get('message', 'Operation stopped.'),
                'cancelled': 'Cancelled. Downloads are kept for retry.'}.get(state, state)
        self.status.setText(text)
        if state == 'files_verified':
            self.ready = True
            if data.get('code'):
                self.active_code = data['code']
            self.persist()
        elif state == 'failed' and self.worker_arguments and self.worker_arguments[0] in {'sync', 'play', 'install', 'launch'}:
            self.ready = False
        elif state == 'nexus_authenticated':
            self.status.setText('Nexus connected. Automatic downloads available.' if data.get('premium')
                                else 'Nexus connected. Automatic downloads currently require Premium.')

    def cancel_job(self):
        if self.process:
            if self.worker_arguments[0] == 'install' and self.package:
                request = Path(self.root.text()).resolve() / 'jobs' / self.package.job_id / 'cancel'
                request.parent.mkdir(parents=True, exist_ok=True)
                request.write_text('cancel\n', encoding='utf-8')
            elif sys.platform != 'win32':
                os.kill(self.process.processId(), signal.SIGINT)
            else:
                self.process.terminate()
            self.status.setText('Cancelling…')

    def worker_error(self, error):
        if self.process:
            self.status.setText(self.process.errorString())
            if error == QProcess.ProcessError.FailedToStart:
                self.release_worker()

    def release_worker(self):
        self.process.deleteLater()
        self.process = None
        self.progress.setRange(0, 100)
        self.update_controls()

    def finished(self, code, _status):
        self.read_output()
        self.release_worker()
        self.progress.setValue(100 if code == 0 else 0)

    def closeEvent(self, event):
        if self.process:
            QMessageBox.information(self, 'Operation running', 'Cancel the operation before closing.')
            event.ignore()
        else:
            self.persist()
            event.accept()


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    app = QApplication.instance() or QApplication([sys.argv[0]])
    window = Window(Path(argv[0]) if argv else None)
    window.show()
    return app.exec()
