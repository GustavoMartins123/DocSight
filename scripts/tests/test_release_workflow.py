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

    def test_workflow_does_not_automatically_publish_or_mutate_git(self):
        self.assertNotIn('contents: write', self.workflow)
        self.assertNotIn('git push', self.workflow)
        self.assertNotIn('gh release create', self.workflow)
        self.assertIn('python -m scripts.release version --tag', self.workflow)


if __name__ == '__main__':
    unittest.main()
