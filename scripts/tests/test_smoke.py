from pathlib import Path
import struct
import subprocess
import sys
from unittest.mock import patch
import unittest
import zlib

from scripts.common import ROOT, ProcessResult, ToolError, json_bytes
from scripts.smoke import SMOKE_CHECKS, extract_verified, native_target, smoke_archive, validate_png
from scripts.tests.test_release import ReleaseFixture


def sample_png():
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xFFFFFFFF)
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(b'\0\0\0\0')) + chunk(b'IEND', b''))


class SmokeTests(ReleaseFixture):
    def setUp(self):
        super().setUp()
        self.calls = []
        self.bad_version = False
        self.bad_png = False
        self.nondeterministic = False

    def engine(self, arguments, **options):
        self.calls.append((arguments, options))
        if '--version' in arguments:
            version = '9.9.9' if self.bad_version else '0.1.4'
            return ProcessResult(0, f'docsight {version}\n'.encode(), b'', 1)
        if 'completions' in arguments:
            return ProcessResult(0, b'docsight shell completion\n', b'', 1)
        if any(argument.endswith('invalid.docx') for argument in arguments):
            return ProcessResult(10, b'', json_bytes({'code': 'UNSUPPORTED_FORMAT'}), 1)
        if 'capabilities' in arguments:
            result = {'commands': [{'name': name} for name in ('inspect', 'render', 'diff', 'find', 'completions')]}
        elif 'inspect' in arguments:
            result = {'format': 'pdf' if arguments[-1].endswith('.pdf') else 'docx', 'pages': 1}
            if self.nondeterministic:
                result['nonce'] = len(self.calls)
        elif 'text' in arguments:
            result = {'blocks': [{'id': 'test', 'text': 'sample text'}]}
        elif 'diff' in arguments:
            result = {'summary': {'semantic_changes': 0}}
        elif 'render' in arguments:
            output = Path(arguments[arguments.index('--out') + 1])
            output.write_bytes(b'invalid PNG' if self.bad_png else sample_png())
            result = {'artifact': 'fixture.png'}
        else:
            raise AssertionError(arguments)
        return ProcessResult(0, json_bytes({'schema': 'docsight.agent/v2', 'result': result}), b'', 1)

    def test_native_smoke_orchestration_exercises_every_required_operation(self):
        archive = self.package()
        with patch('scripts.smoke.native_target', return_value='x86_64-unknown-linux-gnu'):
            report = smoke_archive(archive, self.engine)
        self.assertTrue(report['passed'])
        self.assertEqual([item['name'] for item in report['checks']], list(SMOKE_CHECKS))
        self.assertTrue(any('--sandbox' in arguments for arguments, _ in self.calls))
        for _, options in self.calls:
            self.assertNotIn('cargo', options['env']['PATH'])

    def test_wrong_host_is_not_silently_emulated(self):
        archive = self.package()
        with patch('scripts.smoke.native_target', return_value='aarch64-apple-darwin'):
            with self.assertRaisesRegex(ToolError, 'native target'):
                smoke_archive(archive, self.engine)
        self.assertEqual(self.calls, [])

    def test_version_mismatch_fails_but_other_checks_continue(self):
        self.bad_version = True
        with patch('scripts.smoke.native_target', return_value='x86_64-unknown-linux-gnu'):
            report = smoke_archive(self.package(), self.engine)
        self.assertFalse(report['passed'])
        self.assertEqual(report['checks'][0]['error_code'], 'SMOKE_VERSION_MISMATCH')
        self.assertEqual(len(report['checks']), len(SMOKE_CHECKS))

    def test_nondeterminism_is_reported(self):
        self.nondeterministic = True
        with patch('scripts.smoke.native_target', return_value='x86_64-unknown-linux-gnu'):
            report = smoke_archive(self.package(), self.engine)
        self.assertFalse(report['passed'])
        self.assertTrue(any(item['error_code'] == 'SMOKE_NONDETERMINISTIC' for item in report['checks']))

    def test_invalid_render_is_reported(self):
        self.bad_png = True
        with patch('scripts.smoke.native_target', return_value='x86_64-unknown-linux-gnu'):
            report = smoke_archive(self.package(), self.engine)
        self.assertFalse(report['passed'])
        self.assertTrue(any(item['error_code'] == 'SMOKE_INVALID_PNG' for item in report['checks']))

    def test_extraction_does_not_replace_an_existing_directory(self):
        archive = self.package()
        target = self.root / 'existing'
        target.mkdir()
        with self.assertRaises(FileExistsError):
            extract_verified(archive, target)

    def test_png_crc_and_trailing_bytes_are_checked(self):
        path = self.root / 'test.png'
        path.write_bytes(sample_png())
        validate_png(path)
        for data in (sample_png() + b'x', sample_png()[:-1], sample_png()[:40] + b'corrupt'):
            path.write_bytes(data)
            with self.assertRaises(ToolError):
                validate_png(path)

    def test_cli_does_not_pass_a_header_only_packaging_fixture_as_a_real_engine(self):
        archive = self.package(native_target())
        output = self.root / 'smoke.json'
        result = subprocess.run([sys.executable, '-m', 'scripts.smoke', str(archive), '--out', str(output)],
                                cwd=ROOT, capture_output=True, check=False)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertTrue(output.is_file())
        self.assertIn(b'"passed": false', result.stdout)


if __name__ == '__main__':
    unittest.main()
