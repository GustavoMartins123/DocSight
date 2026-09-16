import copy
import hashlib
import json
from pathlib import Path
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import unittest
import warnings
import zipfile

from scripts.common import ROOT, ToolError, json_bytes
from scripts.release import (
    DOCUMENTS, EXAMPLES, SMOKE_CHECKS, TARGETS, archive_basename, check_binary, checked_revision,
    checked_version, collect_archives, make_package, matrix, safe_member, verify_archive,
)


class ReleaseFixture(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / 'Cargo.toml').write_text('[workspace.package]\nversion = "0.1.4"\n')
        (self.root / 'rust-toolchain.toml').write_text('[toolchain]\nchannel = "1.96.0"\n')
        for name in DOCUMENTS:
            (self.root / name).write_text(name + '\n')
        (self.root / 'schemas/v2').mkdir(parents=True)
        (self.root / 'schemas/v2/agent-envelope.json').write_text('{"type":"object"}\n')
        (self.root / 'fixtures/validation').mkdir(parents=True)
        for name in EXAMPLES:
            (self.root / 'fixtures/validation' / name).write_text('synthetic packaging fixture\n')
        self.notices = self.root / 'notices.md'
        self.notices.write_text('Synthetic third-party notices for packaging tests.\n')
        self.revision = 'a' * 40

    def binary(self, target):
        header = bytearray(128)
        _, kind, machine = TARGETS[target]
        if kind == 'elf':
            header[:6] = b'\x7fELF\x02\x01'
            struct.pack_into('<H', header, 16, 2)
            struct.pack_into('<H', header, 18, machine)
        elif kind == 'macho':
            header[:4] = b'\xcf\xfa\xed\xfe'
            struct.pack_into('<I', header, 4, machine)
            struct.pack_into('<I', header, 12, 2)
        else:
            header[:2] = b'MZ'
            struct.pack_into('<I', header, 60, 64)
            header[64:68] = b'PE\0\0'
            struct.pack_into('<H', header, 68, machine)
            struct.pack_into('<H', header, 88, 0x20B)
        path = self.root / target
        path.write_bytes(header)
        return path

    def package(self, target='x86_64-unknown-linux-gnu', out='dist', revision=None):
        return make_package(self.binary(target), target, revision or self.revision,
                            self.notices, self.root / out, self.root)

    def receipt(self, path):
        manifest = verify_archive(path)
        value = {'schema': 'docsight.release-smoke/v1', 'version': manifest['version'],
                 'target': manifest['target'], 'revision': manifest['revision'],
                 'archive_sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'passed': True,
                 'checks': [{'name': name, 'passed': True, 'error_code': None, 'elapsed_ms': 1} for name in SMOKE_CHECKS]}
        receipt = path.parent / f"smoke-{manifest['target']}.json"
        receipt.write_bytes(json_bytes(value))
        return receipt

    def rewrite(self, path, transform):
        with zipfile.ZipFile(path) as archive:
            entries = [(copy.copy(info), archive.read(info)) for info in archive.infolist()]
        entries = transform(entries)
        with zipfile.ZipFile(path, 'w') as archive:
            for info, data in entries:
                archive.writestr(info, data)


class ReleaseTests(ReleaseFixture):
    def test_all_five_target_headers_are_supported(self):
        for target in TARGETS:
            with self.subTest(target=target):
                check_binary(self.binary(target), target)

    def test_wrong_architecture_fails(self):
        with self.assertRaisesRegex(ToolError, 'architecture'):
            check_binary(self.binary('aarch64-unknown-linux-gnu'), 'x86_64-unknown-linux-gnu')

    def test_library_is_not_an_executable(self):
        target = 'aarch64-apple-darwin'
        path = self.binary(target)
        data = bytearray(path.read_bytes())
        struct.pack_into('<I', data, 12, 6)
        path.write_bytes(data)
        with self.assertRaises(ToolError):
            check_binary(path, target)

    def test_windows_dll_is_rejected(self):
        target = 'x86_64-pc-windows-msvc'
        path = self.binary(target)
        data = bytearray(path.read_bytes())
        struct.pack_into('<H', data, 86, 0x2000)
        path.write_bytes(data)
        with self.assertRaises(ToolError):
            check_binary(path, target)

    def test_zip_is_deterministic_and_preserves_executable_mode(self):
        first = self.package(out='first')
        second = self.package(out='second')
        self.assertEqual(first.read_bytes(), second.read_bytes())
        manifest = verify_archive(first)
        self.assertEqual(manifest['revision'], self.revision)
        with zipfile.ZipFile(first) as archive:
            for info in archive.infolist():
                self.assertEqual(info.date_time, (1980, 1, 1, 0, 0, 0))
                expected = 0o755 if info.filename.endswith('/docsight') else 0o644
                self.assertEqual((info.external_attr >> 16) & 0o777, expected)
        sidecar = first.with_name(first.name + '.sha256').read_text()
        self.assertEqual(sidecar, f'{hashlib.sha256(first.read_bytes()).hexdigest()}  {first.name}\n')

    def test_existing_archives_are_not_overwritten(self):
        path = self.package()
        original = path.read_bytes()
        with self.assertRaises(FileExistsError):
            self.package()
        self.assertEqual(path.read_bytes(), original)

    def test_missing_license_prevents_packaging(self):
        (self.root / 'LICENSE-MIT').unlink()
        with self.assertRaises(ToolError):
            self.package()
        self.assertFalse((self.root / 'dist').exists())

    def test_missing_schemas_prevents_packaging(self):
        shutil.rmtree(self.root / 'schemas')
        with self.assertRaisesRegex(ToolError, 'schemas'):
            self.package()

    def test_payload_tampering_is_detected(self):
        path = self.package()
        def tamper(entries):
            return [(info, b'X' * len(data) if info.filename.endswith('/docsight') else data) for info, data in entries]
        self.rewrite(path, tamper)
        with self.assertRaisesRegex(ToolError, 'digest'):
            verify_archive(path)

    def test_corrupt_deflate_stream_is_a_typed_archive_error(self):
        path = self.package()
        with zipfile.ZipFile(path) as archive:
            info = next(item for item in archive.infolist() if item.filename.endswith('/docsight'))
        data = bytearray(path.read_bytes())
        filename_length, extra_length = struct.unpack_from('<HH', data, info.header_offset + 26)
        start = info.header_offset + 30 + filename_length + extra_length
        data[start] = 7
        path.write_bytes(data)
        with self.assertRaisesRegex(ToolError, 'corrupt'):
            verify_archive(path)

    def test_extra_archive_members_are_rejected(self):
        path = self.package()
        def add(entries):
            info = copy.copy(entries[0][0])
            info.filename = 'extra.txt'
            return entries + [(info, b'extra')]
        self.rewrite(path, add)
        with self.assertRaises(ToolError):
            verify_archive(path)

    def test_duplicate_members_are_rejected(self):
        path = self.package()
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            self.rewrite(path, lambda entries: entries + [entries[0]])
        with self.assertRaises(ToolError):
            verify_archive(path)

    def test_symlink_members_are_rejected(self):
        path = self.package()
        def symlink(entries):
            entries[0][0].external_attr = (stat.S_IFLNK | 0o777) << 16
            return entries
        self.rewrite(path, symlink)
        with self.assertRaises(ToolError):
            verify_archive(path)

    def test_manifest_duplicate_keys_are_rejected(self):
        path = self.package()
        def duplicate(entries):
            return [(info, data.replace(b'"version": "0.1.4"', b'"version": "0.1.4", "version": "9.9.9"')
                     if info.filename.endswith('/release-manifest.json') else data) for info, data in entries]
        self.rewrite(path, duplicate)
        with self.assertRaises(ToolError):
            verify_archive(path)

    def test_renamed_archive_is_rejected(self):
        path = self.package()
        renamed = path.with_name('wrong.zip')
        path.rename(renamed)
        with self.assertRaises(ToolError):
            verify_archive(renamed)

    def test_complete_release_aggregates_sorted_checksums(self):
        for target in TARGETS:
            self.receipt(self.package(target))
        result = collect_archives(self.root / 'dist')
        self.assertEqual(set(result['targets']), set(TARGETS))
        self.assertEqual(result['revision'], self.revision)
        lines = (self.root / 'dist/SHA256SUMS').read_text().splitlines()
        self.assertEqual(len(lines), 5)
        self.assertEqual([line.split('  ')[1] for line in lines], sorted(line.split('  ')[1] for line in lines))

    def test_partial_release_is_rejected(self):
        self.package()
        with self.assertRaisesRegex(ToolError, 'five'):
            collect_archives(self.root / 'dist')

    def test_mixed_revisions_are_rejected(self):
        for index, target in enumerate(TARGETS):
            self.receipt(self.package(target, revision='b' * 40 if index == 0 else self.revision))
        with self.assertRaisesRegex(ToolError, 'same version'):
            collect_archives(self.root / 'dist')

    def test_bad_sidecar_is_rejected(self):
        for target in TARGETS:
            path = self.package(target)
            self.receipt(path)
        path.with_name(path.name + '.sha256').write_text('wrong\n')
        with self.assertRaises(ToolError):
            collect_archives(self.root / 'dist')

    def test_missing_native_receipts_prevent_release_aggregation(self):
        for target in TARGETS:
            self.package(target)
        with self.assertRaises(FileNotFoundError):
            collect_archives(self.root / 'dist')
        self.assertFalse((self.root / 'dist/SHA256SUMS').exists())

    def test_stale_smoke_receipt_is_rejected(self):
        for target in TARGETS:
            receipt = self.receipt(self.package(target))
        value = json.loads(receipt.read_text())
        value['archive_sha256'] = '0' * 64
        receipt.write_bytes(json_bytes(value))
        with self.assertRaises(ToolError):
            collect_archives(self.root / 'dist')

    def test_partial_smoke_receipt_cannot_claim_success(self):
        for target in TARGETS:
            receipt = self.receipt(self.package(target))
        value = json.loads(receipt.read_text())
        value['checks'] = value['checks'][:-1]
        receipt.write_bytes(json_bytes(value))
        with self.assertRaises(ToolError):
            collect_archives(self.root / 'dist')

    def test_cli_verifies_a_real_archive(self):
        path = self.package()
        result = subprocess.run([sys.executable, '-m', 'scripts.release', 'verify', str(path)],
                                cwd=ROOT, capture_output=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)['version'], '0.1.4')

    def test_cli_rejects_a_mismatched_tag_without_stdout(self):
        result = subprocess.run([sys.executable, '-m', 'scripts.release', 'version', '--tag', 'v9.9.9'],
                                cwd=ROOT, capture_output=True, check=False)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, b'')
        self.assertEqual(json.loads(result.stderr)['code'], 'TAG_VERSION_MISMATCH')


class ReleaseContractTests(unittest.TestCase):
    def test_checked_in_matrix_contains_every_target_once(self):
        self.assertEqual({item['target'] for item in matrix()['include']}, set(TARGETS))

    def test_unsafe_or_nonportable_paths_are_rejected(self):
        for path in ('../x', '/tmp/x', 'a/../x', './x', '.', 'a//b', 'a\\b', 'C:x',
                     'a/CON.txt', 'a/nul', 'name.', 'name ', 'file?', 'a\nx'):
            with self.subTest(path=path), self.assertRaises(ToolError):
                safe_member(path)

    def test_versions_and_revisions_cannot_inject_paths_or_shell(self):
        for version in ('01.2.3', '../x', '1.2.3; echo', 'v1.2.3', '1.2', None):
            with self.subTest(version=version), self.assertRaises(ToolError):
                checked_version(version)
        for revision in ('main', 'a' * 39, 'A' * 40, '../x'):
            with self.subTest(revision=revision), self.assertRaises(ToolError):
                checked_revision(revision)
        self.assertEqual(checked_version('1.0.0-rc.1'), '1.0.0-rc.1')


if __name__ == '__main__':
    unittest.main()
