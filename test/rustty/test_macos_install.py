"""Exercise the real just recipe with fake builds in an isolated Applications directory."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


@unittest.skipUnless(sys.platform == 'darwin' and shutil.which('just'), 'requires macOS and just')
class MacOSInstallTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='rustty install ')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.applications = self.root / 'Applications'
        self.applications.mkdir()
        self.app = self.applications / 'Rustty.app'
        justfile = Path(__file__).resolve().parents[2] / 'justfile'
        (self.root / 'justfile').write_text(
            justfile.read_text().replace('/Applications', str(self.applications)))
        commands = self.root / 'bin'
        commands.mkdir()
        scripts = {
            'nu': '''#!/bin/sh
set -eu
printf '%s\\n' "$@" > build-args
test "${FAIL_BUILD:-0}" != 1
mkdir -p target/release/Rustty.app
printf '%s' "${BUNDLE_CONTENTS:-new}" > target/release/Rustty.app/payload
''',
            'codesign': '''#!/bin/sh
set -eu
test "${FAIL_VERIFY:-0}" != 1
test -f "$4/payload"
''',
            'mv': '''#!/bin/sh
set -eu
if [ "${FAIL_PUBLISH:-0}" = 1 ]; then
    case "$1" in */.Rustty-install.*/Rustty.app) exit 1 ;; esac
fi
exec /bin/mv "$@"
''',
        }
        for name, script in scripts.items():
            command = commands / name
            command.write_text(script)
            command.chmod(0o755)
        self.environment = {**os.environ, 'PATH': f'{commands}:{os.environ["PATH"]}'}

    def run_just(self, *arguments, **environment):
        return subprocess.run(
            ['just', *arguments], cwd=self.root,
            env={**self.environment, **environment}, capture_output=True, text=True)

    def test_default_only_lists_recipes(self):
        result = self.run_just()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('rustty-install-macos', result.stdout)
        self.assertFalse((self.root / 'build-args').exists())
        self.assertEqual(list(self.applications.iterdir()), [])

    def test_install_retains_each_previous_bundle(self):
        for content in ('first', 'second', 'third'):
            result = self.run_just('rustty-install-macos', BUNDLE_CONTENTS=content)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((self.app / 'payload').read_text(), content)
        self.assertEqual((self.root / 'build-args').read_text().splitlines(), [
            'crates/rustty-app/build.nu', '--release', '--sign', 'Rustty Local Signing'])
        backups = list(self.applications.glob('Rustty.previous.*.app'))
        self.assertEqual(sorted((backup / 'payload').read_text() for backup in backups),
                         ['first', 'second'])
        self.assertEqual(list(self.applications.glob('.Rustty-install.*')), [])

    def test_failures_preserve_the_installed_bundle(self):
        self.app.mkdir()
        (self.app / 'payload').write_text('old')
        for failure in ('FAIL_BUILD', 'FAIL_VERIFY', 'FAIL_PUBLISH'):
            with self.subTest(failure=failure):
                result = self.run_just('rustty-install-macos', **{failure: '1'})
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual((self.app / 'payload').read_text(), 'old')
                self.assertEqual(list(self.applications.iterdir()), [self.app])


if __name__ == '__main__':
    unittest.main()
