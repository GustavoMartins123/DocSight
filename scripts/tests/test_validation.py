from pathlib import Path
import tempfile
import unittest

from scripts.common import ProcessResult, read_json, sha256_file
from scripts.validate import CHECK_NAMES, commands, run_validation


class ValidationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / 'Cargo.toml').write_text('[workspace.package]\nversion="0.1.4"\n')
        self.output = self.root / 'result'
        self.calls = []
        self.missing_cargo = False
        self.failing_tests = False
        self.dirty = False
        self.revision_changes = False
        self.revision_reads = 0

    def runner(self, arguments, **options):
        self.calls.append(arguments)
        if arguments[:3] == ['git', 'rev-parse', 'HEAD']:
            self.revision_reads += 1
            sha = 'b' if self.revision_changes and self.revision_reads > 1 else 'a'
            return ProcessResult(0, (sha * 40 + '\n').encode(), b'', 1)
        if arguments[:2] == ['git', 'status']:
            return ProcessResult(0, b' M source.rs\n' if self.dirty else b'', b'', 1)
        if arguments[0] == 'cargo' and self.missing_cargo:
            raise FileNotFoundError('cargo')
        if 'unittest' in arguments and self.failing_tests:
            return ProcessResult(1, b'', b'FAILED\n', 10)
        return ProcessResult(0, b'checked\n', b'', 1)

    def test_commands_include_all_rust_and_tooling_gates(self):
        entries = commands(self.output)
        self.assertEqual(tuple(name for name, _ in entries), CHECK_NAMES)
        for name, command in entries:
            if name in ('cargo_clippy', 'cargo_test', 'cargo_build', 'ds9_benchmark'):
                self.assertIn('--locked', command)
        self.assertIn('--all-features', dict(entries)['cargo_test'])

    def test_complete_success_retains_hash_bound_logs(self):
        report = run_validation(self.output, self.root, self.runner)
        self.assertTrue(report['passed'])
        self.assertEqual(len(report['checks']), len(CHECK_NAMES))
        self.assertEqual(read_json(self.output / 'validation.json'), report)
        for check in report['checks']:
            self.assertEqual(sha256_file(self.output / check['stdout_log']), check['stdout_sha256'])
            self.assertEqual(sha256_file(self.output / check['stderr_log']), check['stderr_sha256'])

    def test_missing_cargo_is_blocked_not_passed_and_does_not_skip_other_gates(self):
        self.missing_cargo = True
        report = run_validation(self.output, self.root, self.runner)
        self.assertFalse(report['passed'])
        self.assertEqual(len(report['checks']), len(CHECK_NAMES))
        self.assertEqual(sum(check['status'] == 'blocked' for check in report['checks']), 5)
        self.assertEqual(report['checks'][-1]['status'], 'passed')

    def test_test_failures_are_not_reclassified_as_external_blockers(self):
        self.failing_tests = True
        report = run_validation(self.output, self.root, self.runner)
        self.assertEqual(report['checks'][0]['status'], 'failed')
        self.assertFalse(report['passed'])
        self.assertEqual(report['checks'][-1]['status'], 'passed')

    def test_dirty_worktree_prevents_acceptance(self):
        self.dirty = True
        report = run_validation(self.output, self.root, self.runner)
        self.assertFalse(report['passed'])
        self.assertFalse(report['clean_tree_before'])

    def test_revision_changes_during_validation_prevent_acceptance(self):
        self.revision_changes = True
        report = run_validation(self.output, self.root, self.runner)
        self.assertFalse(report['same_revision_after'])
        self.assertFalse(report['passed'])

    def test_existing_evidence_is_never_overwritten(self):
        self.output.mkdir()
        (self.output / 'validation.json').write_text('original')
        with self.assertRaises(FileExistsError):
            run_validation(self.output, self.root, self.runner)
        self.assertEqual((self.output / 'validation.json').read_text(), 'original')


if __name__ == '__main__':
    unittest.main()
