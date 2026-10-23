"""Exercise the Windows installer with fake builds under target, never on the Desktop."""

import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile


POWERSHELL = shutil.which('powershell.exe')
REPOSITORY = Path(__file__).resolve().parents[2]


@unittest.skipUnless(sys.platform == 'win32' and POWERSHELL,
                     'requires Windows and Windows PowerShell')
class WindowsInstallTests(unittest.TestCase):
    def setUp(self):
        target = REPOSITORY / 'target'
        target.mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix='windows install ', dir=target)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repository = self.root / 'repository'
        self.desktop = self.root / 'Desktop with spaces'
        self.bundle = self.desktop / 'Rustty'
        self.app_source = self.repository / 'crates/rustty-app'
        self.build_output = self.repository / 'target/x86_64-pc-windows-msvc/release'
        self.build_arguments = self.root / 'cargo-args.json'

        # The real packaging script sees a tiny, valid runtime archive. Only
        # its pinned digest changes in this isolated copy; package validation
        # and extraction still run normally, without network access.
        script = (REPOSITORY / 'crates/rustty-app/build.ps1').read_text()
        version = re.search(r"\$version = '([^']+)'", script).group(1)
        self.package = self.repository / (
            f'target/conpty/microsoft.windows.console.conpty.{version}.nupkg')
        self.package.parent.mkdir(parents=True)
        with zipfile.ZipFile(self.package, 'w') as archive:
            archive.writestr('runtimes/win-x64/native/conpty.dll', b'fixture DLL')
            archive.writestr('build/native/runtimes/x64/OpenConsole.exe', b'fixture EXE')
        digest = hashlib.sha256(self.package.read_bytes()).hexdigest().upper()
        script, replaced = re.subn(r"\$sha256 = '[A-F0-9]{64}'",
                                   f"$sha256 = '{digest}'", script)
        self.assertEqual(replaced, 1)
        self.app_source.mkdir(parents=True)
        (self.app_source / 'build.ps1').write_text(script)
        shutil.copyfile(REPOSITORY / 'justfile', self.repository / 'justfile')

        for relative in (
                'LICENSE', 'tmuxx/COPYING',
                'crates/rustty-app/resources/themes/Fixture',
                'crates/rustty-app/resources/ghostty.terminfo',
                'crates/rustty-app/resources/icons/rustty.ico',
                'crates/rustty-app/resources/iTerm2-Color-Schemes-LICENSE.txt',
                'crates/rustty-app/resources/ConPTY-LICENSE.txt',
                'crates/rustty-app/resources/README.md',
                'crates/rustty-font/resources/JetBrainsMono-OFL.txt',
                'crates/rustty-font/resources/NerdFontsSymbols-LICENSE.txt',
                'src/shell-integration/zsh/.zshrc'):
            path = self.repository / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'fixture\r\n')

        self.wrapper = self.root / 'run-build.ps1'
        self.wrapper.write_text(r'''
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
function global:cargo {
    ConvertTo-Json -InputObject @($args) | Set-Content -LiteralPath $env:RUSTTY_TEST_CARGO_ARGS
    if ($env:RUSTTY_TEST_FAIL_BUILD -eq '1') {
        $global:LASTEXITCODE = 71
        return
    }
    New-Item -ItemType Directory -Path $env:RUSTTY_TEST_BUILD_OUTPUT -Force | Out-Null
    foreach ($name in @('rustty', 'tmuxx')) {
        Copy-Item -LiteralPath (Join-Path $env:WINDIR 'System32/ping.exe') -Destination (Join-Path $env:RUSTTY_TEST_BUILD_OUTPUT "$name.exe") -Force
        Set-Content -LiteralPath (Join-Path $env:RUSTTY_TEST_BUILD_OUTPUT "$name.pdb") -Value 'fixture symbols'
    }
    $global:LASTEXITCODE = 0
}
function global:Rename-Item {
    [CmdletBinding()]
    param([string]$LiteralPath, [string]$NewName)
    if ($env:RUSTTY_TEST_FAIL_PUBLISH -eq '1' -and
        [IO.Path]::GetFileName($LiteralPath).StartsWith('.Rustty-install.')) {
        throw 'Injected publication failure'
    }
    Microsoft.PowerShell.Management\Rename-Item -LiteralPath $LiteralPath -NewName $NewName
}
& (Join-Path $env:RUSTTY_TEST_REPOSITORY 'crates/rustty-app/build.ps1') @args
''')
        self.environment = {
            **os.environ,
            'RUSTTY_TEST_REPOSITORY': str(self.repository),
            'RUSTTY_TEST_BUILD_OUTPUT': str(self.build_output),
            'RUSTTY_TEST_CARGO_ARGS': str(self.build_arguments),
            'RUSTTY_TEST_FAIL_BUILD': '0',
            'RUSTTY_TEST_FAIL_PUBLISH': '0',
        }

    def run_build(self, *arguments, **environment):
        return subprocess.run(
            [POWERSHELL, '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File',
             str(self.wrapper), '-Release', '-Offline', *map(str, arguments)],
            cwd=self.repository, env={**self.environment, **environment},
            capture_output=True, text=True)

    def install(self, content='new', **environment):
        (self.app_source / 'resources/themes/Fixture').write_text(content)
        return self.run_build('-Install', '-InstallDirectory', self.bundle, **environment)

    def backups(self):
        return sorted(self.desktop.glob('Rustty.previous.*'))

    def assert_no_stages(self):
        self.assertEqual(list(self.desktop.glob('.Rustty-install.*')), [])

    def assert_success(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def keep_process(self, arguments, cwd):
        process = subprocess.Popen(arguments, cwd=cwd, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL,
                                   creationflags=subprocess.CREATE_NO_WINDOW)

        def stop():
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)

        self.addCleanup(stop)
        return process

    @staticmethod
    def contents(directory):
        return {str(path.relative_to(directory)): path.read_bytes()
                for path in directory.rglob('*') if path.is_file()}

    def test_complete_install_keeps_target_bundle_unchanged(self):
        target_bundle = self.repository / 'target/release/Rustty'
        target_bundle.mkdir(parents=True)
        (target_bundle / 'old').write_text('running from target')
        self.assert_success(self.install())
        self.assertEqual(list(target_bundle.iterdir()), [target_bundle / 'old'])
        for relative in ('rustty.exe', 'tmuxx.exe', 'rustty.pdb', 'tmuxx.pdb',
                         'resources/conpty/conpty.dll', 'resources/conpty/OpenConsole.exe',
                         'resources/themes/Fixture', 'resources/ghostty.terminfo',
                         'resources/rustty.ico', 'resources/licenses/Ghostty-MIT.txt',
                         'resources/licenses/Resources.md', 'README.txt'):
            self.assertTrue((self.bundle / relative).is_file(), relative)
        integration = self.bundle / 'resources/shell-integration/zsh/.zshrc'
        self.assertEqual(integration.read_bytes(), b'fixture\n')
        arguments = self.build_arguments.read_text()
        self.assertIn('--release', arguments)
        self.assertIn('--locked', arguments)
        self.assertIn('--offline', arguments)
        self.assertIn('x86_64-pc-windows-msvc', arguments)
        self.assertEqual(self.backups(), [])
        self.assert_no_stages()

    def test_repeated_install_retains_complete_running_bundle(self):
        self.assert_success(self.install('first'))
        first = self.contents(self.bundle)
        process = self.keep_process([str(self.bundle / 'rustty.exe'), '-t', '127.0.0.1'],
                                    self.root)
        self.assertIsNone(process.poll())
        self.assert_success(self.install('second'))
        second = self.contents(self.bundle)
        self.assert_success(self.install('third'))
        self.assertIsNone(process.poll())
        backups = [self.contents(path) for path in self.backups()]
        self.assertCountEqual(backups, [first, second])
        self.assertEqual((self.bundle / 'resources/themes/Fixture').read_text(), 'third')
        self.assert_no_stages()

    def test_build_and_publish_failures_preserve_previous_bundle(self):
        self.assert_success(self.install('old'))
        previous = self.contents(self.bundle)
        for failure in ('RUSTTY_TEST_FAIL_BUILD', 'RUSTTY_TEST_FAIL_PUBLISH'):
            with self.subTest(failure=failure):
                result = self.install(**{failure: '1'})
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertEqual(self.contents(self.bundle), previous)
                self.assertEqual(self.backups(), [])
                self.assert_no_stages()

    def test_packaging_failure_preserves_previous_bundle(self):
        self.assert_success(self.install('old'))
        previous = self.contents(self.bundle)
        (self.app_source / 'resources/icons/rustty.ico').unlink()
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.contents(self.bundle), previous)
        self.assertEqual(self.backups(), [])
        self.assert_no_stages()

    def test_locked_working_directory_preserves_previous_bundle(self):
        self.assert_success(self.install('old'))
        previous = self.contents(self.bundle)
        process = self.keep_process(
            [sys.executable, '-c', 'import time; time.sleep(120)'], self.bundle)
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('working directory', result.stderr)
        self.assertIsNone(process.poll())
        self.assertEqual(self.contents(self.bundle), previous)
        self.assertEqual(self.backups(), [])
        self.assert_no_stages()

    def test_runtime_file_lock_preserves_previous_bundle_until_released(self):
        import ctypes
        from ctypes import wintypes

        self.assert_success(self.install('old'))
        previous = self.contents(self.bundle)
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                      wintypes.LPVOID, wintypes.DWORD, wintypes.DWORD,
                                      wintypes.HANDLE]
        kernel.CreateFileW.restype = wintypes.HANDLE
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel.CloseHandle.restype = wintypes.BOOL
        # Mirror Rustty's verified ConPTY handles: GENERIC_READ, FILE_SHARE_READ,
        # OPEN_EXISTING. The runtime denies deletion for its entire lifetime.
        handle = kernel.CreateFileW(str(self.bundle / 'resources/conpty/conpty.dll'),
                                    0x80000000, 1, None, 3, 0x80, None)
        if handle == wintypes.HANDLE(-1).value:
            self.fail(str(ctypes.WinError(ctypes.get_last_error())))
        try:
            result = self.install()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('holding a file open', result.stderr)
            self.assertEqual(self.contents(self.bundle), previous)
            self.assertEqual(self.backups(), [])
            self.assert_no_stages()
        finally:
            kernel.CloseHandle(handle)
        self.assert_success(self.install('after release'))
        self.assertEqual([self.contents(path) for path in self.backups()], [previous])
        self.assert_no_stages()

    def test_junction_destination_is_rejected(self):
        outside = self.root / 'unrelated files'
        outside.mkdir()
        (outside / 'keep').write_text('unchanged')
        self.desktop.mkdir()
        self.create_junction(self.bundle, outside)
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('junction', result.stderr)
        self.assertEqual((outside / 'keep').read_text(), 'unchanged')
        self.assertFalse(self.build_arguments.exists())
        self.assert_no_stages()

    def test_nested_junction_is_rejected_without_traversal(self):
        self.assert_success(self.install())
        outside = self.root / 'unrelated files'
        outside.mkdir()
        (outside / 'keep').write_text('unchanged')
        self.create_junction(self.bundle / 'extra', outside)
        self.build_arguments.unlink()
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('junction', result.stderr)
        self.assertEqual(list(outside.iterdir()), [outside / 'keep'])
        self.assertEqual((outside / 'keep').read_text(), 'unchanged')
        self.assertTrue((self.bundle / 'rustty.exe').is_file())
        self.assertFalse(self.build_arguments.exists())
        self.assertEqual(self.backups(), [])
        self.assert_no_stages()

    def create_junction(self, path, target):
        command = ('New-Item -ItemType Junction -Path ' + self.quote(path)
                   + ' -Target ' + self.quote(target) + ' | Out-Null')
        result = subprocess.run(
            [POWERSHELL, '-NoProfile', '-Command', command], capture_output=True, text=True)
        self.assert_success(result)

    @staticmethod
    def quote(path):
        return "'" + str(path).replace("'", "''") + "'"

    def test_destination_requires_install_switch(self):
        result = self.run_build('-InstallDirectory', self.bundle)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('requires -Install', result.stderr)
        self.assertFalse(self.build_arguments.exists())
        self.assertFalse(self.desktop.exists())

    def test_destination_with_trailing_separator(self):
        self.assert_success(self.run_build('-Install', '-InstallDirectory',
                                           str(self.bundle) + '\\'))
        self.assertTrue((self.bundle / 'rustty.exe').is_file())
        self.assert_no_stages()

    def test_ordinary_build_still_packages_under_target(self):
        self.assert_success(self.run_build())
        target_bundle = self.repository / 'target/release/Rustty'
        self.assertTrue((target_bundle / 'rustty.exe').is_file())
        self.assertFalse(self.desktop.exists())

    @unittest.skipUnless(shutil.which('just'), 'requires just')
    def test_just_default_and_windows_recipe(self):
        for arguments in ([], ['--dry-run', 'rustty-install-windows']):
            result = subprocess.run(['just', *arguments], cwd=self.repository,
                                    capture_output=True, text=True)
            self.assert_success(result)
            if arguments:
                self.assertIn('build.ps1 -Release -Install', result.stderr)
                self.assertIn('powershell.exe -NoProfile', result.stderr)
            else:
                self.assertIn('rustty-install-windows', result.stdout)
        self.assertFalse(self.build_arguments.exists())
        self.assertFalse(self.desktop.exists())


if __name__ == '__main__':
    unittest.main()
