import copy
from pathlib import Path
import tempfile
import unittest

from scripts.common import ToolError
from scripts.notices import generate_notices


class NoticeTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / 'Cargo.toml').write_text('[package]\nname="dependency"\nversion="1.0.0"\n')
        (self.root / 'LICENSE').write_text('Synthetic license text for tests.\n')
        self.lock = self.root / 'Cargo.lock'
        self.lock.write_text('version=4\n[[package]]\nname="dependency"\nversion="1.0.0"\n')
        self.package = {'id': 'dependency-1', 'name': 'dependency', 'version': '1.0.0',
                        'manifest_path': str(self.root / 'Cargo.toml'), 'license': 'MIT', 'license_file': None}
        self.metadata = {'version': 1, 'packages': [self.package], 'workspace_members': [], 'resolve': {'nodes': []}}

    def test_notices_are_deterministic_and_omit_host_paths(self):
        output = generate_notices(self.metadata, self.lock)
        self.assertEqual(output, generate_notices(self.metadata, self.lock))
        self.assertIn('dependency 1.0.0', output)
        self.assertIn('Synthetic license text', output)
        self.assertNotIn(str(self.root), output)

    def test_no_deps_metadata_is_rejected(self):
        self.metadata['resolve'] = None
        with self.assertRaises(ToolError):
            generate_notices(self.metadata, self.lock)

    def test_unlocked_dependency_is_rejected(self):
        self.package['version'] = '2.0.0'
        with self.assertRaisesRegex(ToolError, 'Cargo.lock'):
            generate_notices(self.metadata, self.lock)

    def test_missing_license_text_is_rejected(self):
        (self.root / 'LICENSE').unlink()
        with self.assertRaisesRegex(ToolError, 'license texts'):
            generate_notices(self.metadata, self.lock)

    def test_missing_license_metadata_is_rejected(self):
        self.package['license'] = None
        with self.assertRaises(ToolError):
            generate_notices(self.metadata, self.lock)

    def test_license_file_cannot_escape_package(self):
        self.package['license_file'] = '../private.txt'
        with self.assertRaises(ToolError):
            generate_notices(self.metadata, self.lock)

    def test_duplicate_dependency_identity_is_rejected(self):
        self.metadata['packages'].append(copy.deepcopy(self.package))
        with self.assertRaises(ToolError):
            generate_notices(self.metadata, self.lock)

    def test_workspace_members_are_not_misrepresented_as_third_party(self):
        self.metadata['workspace_members'] = ['dependency-1']
        with self.assertRaisesRegex(ToolError, 'empty'):
            generate_notices(self.metadata, self.lock)

    def test_declared_custom_license_file_is_included(self):
        (self.root / 'terms.txt').write_text('Declared custom license text.\n')
        self.package['license'] = None
        self.package['license_file'] = 'terms.txt'
        self.assertIn('Declared custom license text', generate_notices(self.metadata, self.lock))


if __name__ == '__main__':
    unittest.main()
