from __future__ import annotations

import argparse
from pathlib import Path
import re
import sys
from typing import Any

from scripts.common import ROOT, ToolError, error_json, run_bounded, write_new
from scripts.release import checked_revision, checked_version, workspace_version

GROUPS = ('Features', 'Fixes', 'Performance', 'Tests', 'Documentation', 'Build and maintenance', 'Other')
KINDS = {'feat': 'Features', 'fix': 'Fixes', 'perf': 'Performance', 'test': 'Tests',
         'docs': 'Documentation', 'build': 'Build and maintenance', 'ci': 'Build and maintenance',
         'chore': 'Build and maintenance', 'refactor': 'Build and maintenance'}


def render_notes(records: list[tuple[str, str]], version: str, revision: str) -> str:
    checked_version(version)
    checked_revision(revision)
    groups: dict[str, list[str]] = {group: [] for group in GROUPS}
    for sha, subject in records:
        checked_revision(sha)
        if not subject or '\n' in subject or '\r' in subject:
            raise ToolError('INVALID_COMMIT_SUBJECT', 'Commit subjects must be nonempty single lines')
        match = re.match(r'^([a-z]+)(?:\([^)]*\))?!?:\s+', subject)
        group = KINDS.get(match.group(1), 'Other') if match else 'Other'
        escaped = re.sub(r'([\\`*{}\[\]()<>_#])', r'\\\1', subject)
        groups[group].append(f'- {escaped} ({sha[:12]})')
    lines = [f'# DocSight {version}', '', f'Candidate commit: `{revision}`.', '',
             'Generated from non-merge Git commits. Publishing requires the release validation gates.', '']
    for group, entries in groups.items():
        if entries:
            lines += [f'## {group}', '', *entries, '']
    if not records:
        lines += ['No non-merge commits exist in the requested range.', '']
    return '\n'.join(lines)


def generate_notes(root: Path, version: str, revision: str, since: str | None = None) -> str:
    checked_revision(revision)
    if since is not None:
        checked_revision(since)
        ancestry = run_bounded(['git', 'merge-base', '--is-ancestor', since, revision], cwd=root)
        if ancestry.returncode != 0 or ancestry.termination:
            raise ToolError('INVALID_CHANGELOG_RANGE', 'Previous revision must be an ancestor of the candidate')
    selection = f'{since}..{revision}' if since is not None else revision
    result = run_bounded(['git', 'log', '--reverse', '--no-merges', '--format=%H%x00%s', selection, '--'],
                         cwd=root, output_limit=2_097_152)
    if result.returncode != 0 or result.termination:
        raise ToolError('GIT_HISTORY_UNAVAILABLE', 'Cannot read the requested bounded Git history')
    records = []
    for line in result.stdout.decode('utf-8').splitlines():
        fields = line.split('\0', 1)
        if len(fields) != 2:
            raise ToolError('INVALID_GIT_OUTPUT', 'Git history output does not match the requested format')
        records.append((fields[0], fields[1]))
    return render_notes(records, version, revision)


def main() -> int:
    parser = argparse.ArgumentParser(description='Generate release notes from verified local Git history')
    parser.add_argument('--revision', required=True)
    parser.add_argument('--since')
    parser.add_argument('--out', type=Path, required=True)
    arguments = parser.parse_args()
    try:
        output = generate_notes(ROOT, workspace_version(), arguments.revision, arguments.since)
        write_new(arguments.out, output.encode(), 0o644)
        return 0
    except (ToolError, OSError, ValueError) as error:
        sys.stderr.buffer.write(error_json(error))
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
