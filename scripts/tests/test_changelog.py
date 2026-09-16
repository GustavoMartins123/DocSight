from pathlib import Path
import subprocess
import tempfile
import unittest

from scripts.changelog import generate_notes, render_notes
from scripts.common import ToolError


class ChangelogTests(unittest.TestCase):
    def test_conventional_commits_are_grouped_deterministically(self):
        records = [('a' * 40, 'fix(parser): reject invalid stream'), ('b' * 40, 'feat(cli): package archives')]
        text = render_notes(records, '0.1.4', 'c' * 40)
        self.assertEqual(text, render_notes(records, '0.1.4', 'c' * 40))
        self.assertLess(text.index('## Features'), text.index('## Fixes'))
        self.assertIn('aaaaaaaaaaaa', text)

    def test_markdown_from_commit_subjects_is_escaped(self):
        text = render_notes([('a' * 40, 'fix: [text](unsafe) <tag>')], '0.1.4', 'b' * 40)
        self.assertIn(r'\[text\]\(unsafe\)', text)
        self.assertNotIn('<tag>', text)

    def test_invalid_subject_is_rejected(self):
        with self.assertRaises(ToolError):
            render_notes([('a' * 40, 'fix: first\nsecond')], '0.1.4', 'b' * 40)

    def test_real_git_range_excludes_previous_revision(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*arguments):
                result = subprocess.run(['git', *arguments], cwd=root, check=True, capture_output=True, text=True)
                return result.stdout.strip()
            git('init', '-b', 'main')
            git('config', 'user.name', 'Test')
            git('config', 'user.email', 'test@local.invalid')
            git('commit', '--allow-empty', '-m', 'chore: baseline')
            baseline = git('rev-parse', 'HEAD')
            git('commit', '--allow-empty', '-m', 'fix(cli): preserve output')
            revision = git('rev-parse', 'HEAD')
            notes = generate_notes(root, '0.1.4', revision, baseline)
            self.assertIn('preserve output', notes)
            self.assertNotIn('chore: baseline', notes)
            with self.assertRaises(ToolError):
                generate_notes(root, '0.1.4', baseline, revision)


if __name__ == '__main__':
    unittest.main()
