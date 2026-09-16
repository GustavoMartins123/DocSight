from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import platform
import shutil
import struct
import sys
import tempfile
from typing import Any, Callable
import zipfile
import zlib

from scripts.common import (
    ProcessResult, ToolError, error_json, isolated_environment, json_bytes,
    parse_json, run_bounded, sha256_file, write_new,
)
from scripts.release import SMOKE_CHECKS, TARGETS, archive_basename, check_binary, verify_archive


def native_target() -> str:
    system = platform.system().lower()
    machine = platform.machine().lower()
    mapping = {
        ('windows', 'amd64'): 'x86_64-pc-windows-msvc',
        ('windows', 'x86_64'): 'x86_64-pc-windows-msvc',
        ('linux', 'x86_64'): 'x86_64-unknown-linux-gnu',
        ('linux', 'aarch64'): 'aarch64-unknown-linux-gnu',
        ('darwin', 'x86_64'): 'x86_64-apple-darwin',
        ('darwin', 'arm64'): 'aarch64-apple-darwin',
    }
    if (system, machine) not in mapping:
        raise ToolError('UNSUPPORTED_HOST', 'Native release tests require a supported operating system and architecture')
    return mapping[system, machine]


def extract_verified(archive_path: Path, destination: Path) -> tuple[Path, dict[str, Any]]:
    manifest = verify_archive(archive_path)
    basename = archive_basename(manifest['version'], manifest['target'])
    destination.mkdir()
    with zipfile.ZipFile(archive_path) as archive:
        for record in manifest['files']:
            output_path = destination / record['path']
            output_path.parent.mkdir(parents=True, exist_ok=True)
            digest = hashlib.sha256()
            size = 0
            with archive.open(f"{basename}/{record['path']}") as source, output_path.open('xb') as output:
                for chunk in iter(lambda: source.read(1_048_576), b''):
                    size += len(chunk)
                    if size > record['size']:
                        raise ToolError('ARCHIVE_CHANGED', 'Archive changed after verification')
                    digest.update(chunk)
                    output.write(chunk)
            if size != record['size'] or digest.hexdigest() != record['sha256']:
                raise ToolError('ARCHIVE_CHANGED', 'Archive changed after verification')
            output_path.chmod(0o755 if record['executable'] else 0o644)
    binary = destination / TARGETS[manifest['target']][0]
    check_binary(binary, manifest['target'])
    return binary, manifest


def agent_result(output: ProcessResult) -> dict[str, Any]:
    envelope = parse_json(output.stdout)
    if not isinstance(envelope, dict) or envelope.get('schema') != 'docsight.agent/v2' or not isinstance(envelope.get('result'), dict):
        raise ToolError('SMOKE_PROTOCOL_MISMATCH', 'Command did not return the canonical agent envelope')
    if output.stderr:
        raise ToolError('SMOKE_STDERR_CONTAMINATION', 'Successful agent command wrote unexpected stderr')
    return envelope['result']


def validate_png(path: Path) -> None:
    with path.open('rb') as source:
        data = source.read(67_108_865)
    if len(data) > 67_108_864 or data[:8] != b'\x89PNG\r\n\x1a\n':
        raise ToolError('SMOKE_INVALID_PNG', 'Render did not produce a bounded PNG')
    offset = 8
    kinds = []
    while offset + 12 <= len(data):
        size = struct.unpack_from('>I', data, offset)[0]
        if size > len(data) - offset - 12:
            raise ToolError('SMOKE_INVALID_PNG', 'PNG contains an incomplete chunk')
        kind = data[offset + 4:offset + 8]
        chunk = data[offset + 8:offset + 8 + size]
        crc = struct.unpack_from('>I', data, offset + 8 + size)[0]
        if zlib.crc32(kind + chunk) & 0xFFFFFFFF != crc:
            raise ToolError('SMOKE_INVALID_PNG', 'PNG chunk checksum is invalid')
        if kind == b'IHDR':
            if kinds or size != 13:
                raise ToolError('SMOKE_INVALID_PNG', 'PNG header is not canonical')
            width, height = struct.unpack_from('>II', chunk)
            if not 0 < width <= 16384 or not 0 < height <= 16384:
                raise ToolError('SMOKE_INVALID_PNG', 'PNG dimensions are outside the smoke-test bounds')
        kinds.append(kind)
        offset += size + 12
        if kind == b'IEND':
            if size != 0 or offset != len(data):
                raise ToolError('SMOKE_INVALID_PNG', 'PNG has trailing data or an invalid end chunk')
            break
    if not kinds or kinds[0] != b'IHDR' or kinds[-1] != b'IEND' or b'IDAT' not in kinds:
        raise ToolError('SMOKE_INVALID_PNG', 'PNG is missing required chunks')


def run_checks(binary: Path, package: Path, work: Path, version: str,
               runner: Callable[..., ProcessResult] = run_bounded) -> list[dict[str, Any]]:
    checks = []
    baselines: dict[str, bytes] = {}
    environment = isolated_environment()

    def check(name: str, arguments: list[str], validate: Callable[[ProcessResult], None], expected: int = 0):
        result = None
        code = None
        try:
            result = runner([str(binary), *arguments], cwd=work, timeout=45, output_limit=8_388_608, env=environment)
            if result.termination:
                raise ToolError('SMOKE_PROCESS_LIMIT', 'Smoke command exceeded its process budget')
            if result.returncode != expected:
                raise ToolError('SMOKE_EXIT_CODE', 'Smoke command returned an unexpected exit code')
            validate(result)
        except (ToolError, OSError, ValueError, KeyError) as error:
            code = error.code if isinstance(error, ToolError) else 'SMOKE_IO_ERROR'
        checks.append({'name': name, 'passed': code is None, 'error_code': code,
                       'elapsed_ms': result.elapsed_ms if result is not None else 0})

    def version_ok(result):
        if result.stdout.decode().strip() != f'docsight {version}' or result.stderr:
            raise ToolError('SMOKE_VERSION_MISMATCH', 'Packaged executable version differs from its manifest')

    def capabilities_ok(result):
        commands = agent_result(result).get('commands')
        if not isinstance(commands, list):
            raise ToolError('SMOKE_CAPABILITIES', 'Capabilities did not declare commands')
        names = {item.get('name') for item in commands if isinstance(item, dict)}
        if not {'inspect', 'render', 'diff', 'find', 'completions'} <= names:
            raise ToolError('SMOKE_CAPABILITIES', 'Capabilities omitted required commands')

    check('version', ['--version'], version_ok)
    check('capabilities', ['--agent', 'capabilities'], capabilities_ok)
    paths = {'docx': package / 'examples/sample_headings.docx', 'pdf': package / 'examples/sample_semantic.pdf'}
    for kind, path in paths.items():
        def inspect_ok(result, kind=kind):
            value = agent_result(result)
            if value.get('format') != kind or type(value.get('pages')) is not int or value['pages'] <= 0:
                raise ToolError('SMOKE_INSPECT', 'Inspection did not recognize the packaged sample')
            baselines[kind] = result.stdout
        check(f'{kind}_inspect', ['--agent', 'inspect', str(path)], inspect_ok)
    for kind, path in paths.items():
        def deterministic(result, kind=kind):
            if result.stdout != baselines.get(kind):
                raise ToolError('SMOKE_NONDETERMINISTIC', 'Repeated inspection changed its output')
        check(f'{kind}_determinism', ['--agent', 'inspect', str(path)], deterministic)
    for kind, path in paths.items():
        def text_ok(result):
            blocks = agent_result(result).get('blocks')
            if not isinstance(blocks, list) or not any(isinstance(block, dict) and block.get('text') for block in blocks):
                raise ToolError('SMOKE_EMPTY_TEXT', 'Text extraction did not return sample content')
        check(f'{kind}_text', ['--agent', 'text', str(path)], text_ok)
    for kind, path in paths.items():
        output_path = work / f'{kind}.png'
        def render_ok(result, output_path=output_path):
            agent_result(result)
            validate_png(output_path)
        check(f'{kind}_render', ['--agent', 'render', str(path), '--page', '1', '--dpi', '72', '--out', str(output_path)], render_ok)

    def diff_ok(result):
        if agent_result(result).get('summary', {}).get('semantic_changes') != 0:
            raise ToolError('SMOKE_IDENTICAL_DIFF', 'Identical documents produced semantic changes')
    check('identical_diff', ['--agent', 'diff', str(paths['docx']), str(paths['docx'])], diff_ok)

    def sandbox_ok(result):
        if agent_result(result).get('format') != 'docx':
            raise ToolError('SMOKE_SANDBOX', 'Sandboxed inspection did not recognize the document')
    check('sandbox_inspect', ['--agent', '--sandbox', 'inspect', str(paths['docx'])], sandbox_ok)
    invalid = work / 'invalid.docx'
    invalid.write_bytes(b'not a supported document')

    def error_ok(result):
        value = parse_json(result.stderr)
        if result.stdout or not isinstance(value, dict) or value.get('code') != 'UNSUPPORTED_FORMAT':
            raise ToolError('SMOKE_ERROR_CONTRACT', 'Invalid input did not preserve stdout and the typed error contract')
    check('typed_error', ['--json-errors', 'inspect', str(invalid), '--json'], error_ok, 10)
    for shell in ('bash', 'elvish', 'fish', 'powershell', 'zsh'):
        def completion_ok(result):
            if b'docsight' not in result.stdout.lower() or result.stderr:
                raise ToolError('SMOKE_COMPLETION', 'Shell completion output is missing or contaminated')
        check(f'completion_{shell}', ['completions', shell], completion_ok)
    return checks


def smoke_archive(archive_path: Path, runner: Callable[..., ProcessResult] = run_bounded) -> dict[str, Any]:
    manifest = verify_archive(archive_path)
    if manifest['target'] != native_target():
        raise ToolError('SMOKE_HOST_MISMATCH', 'Archive must be executed on its native target')
    with tempfile.TemporaryDirectory(prefix='docsight-smoke-') as directory:
        root = Path(directory)
        binary, manifest = extract_verified(archive_path, root / 'package')
        work = root / 'work'
        work.mkdir()
        checks = run_checks(binary, root / 'package', work, manifest['version'], runner)
    return {'schema': 'docsight.release-smoke/v1', 'version': manifest['version'],
            'target': manifest['target'], 'revision': manifest['revision'],
            'archive_sha256': sha256_file(archive_path), 'checks': checks,
            'passed': len(checks) == len(SMOKE_CHECKS) and all(check['passed'] for check in checks)}


def main() -> int:
    parser = argparse.ArgumentParser(description='Exercise an extracted native DocSight release without Rust on PATH')
    parser.add_argument('archive', type=Path)
    parser.add_argument('--out', type=Path, required=True)
    arguments = parser.parse_args()
    try:
        report = smoke_archive(arguments.archive)
        write_new(arguments.out, json_bytes(report))
        sys.stdout.buffer.write(json_bytes(report))
        return 0 if report['passed'] else 1
    except (ToolError, OSError, ValueError) as error:
        sys.stderr.buffer.write(error_json(error))
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
