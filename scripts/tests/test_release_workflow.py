from pathlib import Path
import unittest

from scripts.common import ROOT
from scripts.release import matrix


class ReleaseWorkflowTests(unittest.TestCase):
    def setUp(self):
        self.workflow = (ROOT / '.github/workflows/release.yml').read_text()

    def test_every_platform_runs_natively(self):
        self.assertIn('runs-on: ${{ matrix.runner }}', self.workflow)
        self.assertEqual(len(matrix()['include']), 5)
        for entry in matrix()['include']:
            self.assertNotIn('latest', entry['runner'])

    def test_tests_lint_and_build_use_the_lockfile(self):
        for command in ('cargo clippy --locked', 'cargo test --locked', 'cargo build --locked', 'cargo metadata --locked'):
            self.assertIn(command, self.workflow)
        self.assertIn('cargo fmt --all --check', self.workflow)
        self.assertIn('python -m unittest discover -s scripts/tests -v', self.workflow)

    def test_packaging_requires_native_smoke_and_performance(self):
        self.assertIn('python -m scripts.smoke', self.workflow)
        self.assertIn('benchmark --check', self.workflow)
        self.assertIn('python -m scripts.release collect dist', self.workflow)
        self.assertIn('needs: [configuration, package]', self.workflow)

    def test_corpus_execution_and_manifest_are_preserved_in_artifacts(self):
        self.assertIn('python -m scripts.corpus run', self.workflow)
        self.assertIn('dist/corpus-*.json', self.workflow)
        self.assertIn('cp release/corpus.json dist/corpus-manifest.json', self.workflow)
        conformance = (ROOT / '.github/workflows/conformance.yml').read_text()
        self.assertIn('python -m unittest discover -s scripts/tests -v', conformance)
        self.assertIn('python -m scripts.corpus validate', conformance)

    def test_global_flags_do_not_hide_windows_static_crt(self):
        self.assertNotIn('CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS', self.workflow)
        self.assertIn("matrix.target == 'x86_64-pc-windows-msvc'", self.workflow)
        self.assertIn("'-D warnings -C target-feature=+crt-static'", self.workflow)

    def test_workflow_does_not_automatically_publish_or_mutate_git(self):
        self.assertNotIn('contents: write', self.workflow)
        self.assertNotIn('git push', self.workflow)
        self.assertNotIn('gh release create', self.workflow)
        self.assertIn('python -m scripts.release version --tag', self.workflow)


if __name__ == '__main__':
    unittest.main()
