from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import sys
from typing import Any, Callable

from scripts.common import ROOT, ProcessResult, ToolError, error_json, json_bytes, run_bounded, write_new
from scripts.release import checked_revision, workspace_version

CHECK_NAMES = ('python_tests', 'python_syntax', 'corpus_inventory', 'cargo_fmt', 'cargo_clippy',
               'cargo_test', 'cargo_build', 'ds9_benchmark', 'git_diff_check')


def commands(output: Path) -> list[tuple[str, list[str]]]:
    return [
        ('python_tests', [sys.executable, '-m', 'unittest', 'discover', '-s', 'scripts/tests', '-v']),
        ('python_syntax', [sys.executable, '-m', 'compileall', '-q', 'scripts']),
        ('corpus_inventory', [sys.executable, '-m', 'scripts.corpus', 'validate']),
        ('cargo_fmt', ['cargo', 'fmt', '--all', '--check']),
        ('cargo_clippy', ['cargo', 'clippy', '--locked', '--workspace', '--all-targets', '--all-features', '--', '-D', 'warnings']),
        ('cargo_test', ['cargo', 'test', '--locked', '--workspace', '--all-features']),
        ('cargo_build', ['cargo', 'build', '--locked', '--release', '--workspace', '--all-features']),
        ('ds9_benchmark', ['cargo', 'run', '--locked', '--release', '-p', 'xtask', '--', 'benchmark', '--check',
                           '--output', str(output / 'ds9-performance.json')]),
        ('git_diff_check', ['git', 'diff', '--check']),
    ]


def git_state(root: Path, runner: Callable[..., ProcessResult]) -> tuple[str, bool]:
    revision = runner(['git', 'rev-parse', 'HEAD'], cwd=root, timeout=10, output_limit=4096)
    status = runner(['git', 'status', '--porcelain', '--untracked-files=all'], cwd=root, timeout=10, output_limit=1_048_576)
    if revision.returncode or status.returncode or revision.termination or status.termination:
        raise ToolError('GIT_STATE_UNAVAILABLE', 'Validation requires a readable Git revision and working tree')
    return checked_revision(revision.stdout.decode().strip()), not bool(status.stdout)


def run_validation(output: Path, root: Path = ROOT,
                   runner: Callable[..., ProcessResult] = run_bounded) -> dict[str, Any]:
    revision, clean_before = git_state(root, runner)
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    checks = []
    for name, arguments in commands(output):
        try:
            result = runner(arguments, cwd=root, timeout=3600, output_limit=16_777_216)
            state = 'passed' if result.returncode == 0 and result.termination is None else 'failed'
            reason = result.termination
        except FileNotFoundError:
            result = ProcessResult(127, b'', b'Required executable is unavailable in this environment.\n', 0)
            state, reason = 'blocked', 'executable_missing'
        except OSError:
            result = ProcessResult(126, b'', b'Process could not start; check toolchain and permissions.\n', 0)
            state, reason = 'blocked', 'process_unavailable'
        out_name, err_name = f'{name}.stdout.log', f'{name}.stderr.log'
        write_new(output / out_name, result.stdout)
        write_new(output / err_name, result.stderr)
        checks.append({'name': name, 'status': state, 'reason': reason,
                       'exit_code': result.returncode, 'elapsed_ms': result.elapsed_ms,
                       'stdout_log': out_name, 'stderr_log': err_name,
                       'stdout_sha256': hashlib.sha256(result.stdout).hexdigest(),
                       'stderr_sha256': hashlib.sha256(result.stderr).hexdigest()})
    final_revision, clean_after = git_state(root, runner)
    report = {'schema': 'docsight.validation/v1', 'version': workspace_version(root), 'revision': revision,
              'clean_tree_before': clean_before, 'clean_tree_after': clean_after,
              'same_revision_after': revision == final_revision, 'checks': checks,
              'passed': clean_before and clean_after and revision == final_revision
                        and all(check['status'] == 'passed' for check in checks)}
    write_new(output / 'validation.json', json_bytes(report))
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description='Execute every local validation gate and retain failures and unavailable tools')
    parser.add_argument('--out', type=Path, required=True)
    arguments = parser.parse_args()
    try:
        report = run_validation(arguments.out.resolve())
        sys.stdout.buffer.write(json_bytes(report))
        return 0 if report['passed'] else 1
    except (ToolError, OSError, ValueError) as error:
        sys.stderr.buffer.write(error_json(error))
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
