from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import re
import sys
import tempfile
from typing import Any, Callable

from scripts.common import (
    ROOT, ProcessResult, ToolError, bounded_integer, error_json, exact_keys,
    isolated_environment, json_bytes, parse_json, read_json, run_bounded, sha256_file, write_new,
)
from scripts.release import safe_member, verify_archive
from scripts.smoke import extract_verified, native_target, validate_png

OPERATIONS = ('inspect', 'text', 'render', 'diff')
ORIGINS = ('synthetic', 'consented-real')
FORMATS = ('docx', 'pdf', 'invalid')


def load_manifest(path: Path, root: Path) -> dict[str, Any]:
    manifest = read_json(path)
    exact_keys(manifest, {'schema', 'cases'}, 'corpus manifest')
    if manifest['schema'] != 'docsight.corpus/v1' or not isinstance(manifest['cases'], list):
        raise ToolError('INVALID_CORPUS_MANIFEST', 'A versioned corpus manifest is required')
    bounded_integer(len(manifest['cases']), 1, 10_000, 'case count')
    identities = set()
    base = root.resolve(strict=True)
    for case in manifest['cases']:
        exact_keys(case, {'id', 'file', 'sha256', 'origin', 'format', 'operation', 'expected'}, 'corpus case')
        identity = case['id']
        if not isinstance(identity, str) or not re.fullmatch(r'[a-z0-9][a-z0-9-]{0,79}', identity) or identity in identities:
            raise ToolError('INVALID_CORPUS_ID', 'Case identifiers must be unique portable slugs')
        identities.add(identity)
        for key, choices in (('origin', ORIGINS), ('format', FORMATS), ('operation', OPERATIONS)):
            if case[key] not in choices:
                raise ToolError('INVALID_CORPUS_VALUE', f'Unsupported corpus {key}')
        member = safe_member(case['file'])
        selected = base / member
        if (not selected.resolve(strict=True).is_relative_to(base) or not selected.is_file()
                or any(part.is_symlink() for part in [selected, *selected.parents] if part != base and part.is_relative_to(base))):
            raise ToolError('INVALID_CORPUS_PATH', 'Corpus input must be a regular contained file without symlinks')
        if selected.stat().st_size > 268_435_456:
            raise ToolError('CORPUS_INPUT_LIMIT', 'Corpus documents are limited to 256 MiB')
        if case['sha256'] != sha256_file(selected):
            raise ToolError('CORPUS_DIGEST_MISMATCH', 'Corpus file differs from its reviewed digest')
        expected = exact_keys(case['expected'], {'exit_code', 'diagnostic_codes', 'pointer_equals', 'repeat'}, 'case expectation')
        bounded_integer(expected['exit_code'], 0, 255, 'expected exit code')
        bounded_integer(expected['repeat'], 1, 3, 'repeat count')
        codes = expected['diagnostic_codes']
        if (not isinstance(codes, list) or not all(isinstance(code, str) and re.fullmatch(r'[A-Z][A-Z0-9_]{2,79}', code) for code in codes)
                or len(codes) > 128 or codes != sorted(set(codes))):
            raise ToolError('INVALID_CORPUS_DIAGNOSTICS', 'Expected diagnostics must be sorted unique codes')
        pointers = expected['pointer_equals']
        if not isinstance(pointers, dict) or len(pointers) > 64:
            raise ToolError('INVALID_CORPUS_ASSERTIONS', 'Cases require bounded JSON-pointer assertions')
        for pointer, value in pointers.items():
            if (not isinstance(pointer, str) or not pointer.startswith('/') or len(pointer) > 512
                    or re.search(r'~(?![01])', pointer) or type(value) not in (str, int, float, bool, type(None))):
                raise ToolError('INVALID_CORPUS_ASSERTIONS', 'Assertions must compare JSON pointers with scalar values')
        if expected['exit_code'] != 0 and not codes:
            raise ToolError('MISSING_CORPUS_ERROR', 'Negative cases require an expected diagnostic, not just a failing exit')
    return manifest


def json_pointer(value: Any, pointer: str) -> Any:
    for token in pointer[1:].split('/'):
        token = token.replace('~1', '/').replace('~0', '~')
        if isinstance(value, dict) and token in value:
            value = value[token]
        elif isinstance(value, list) and re.fullmatch(r'0|[1-9][0-9]*', token) and int(token) < len(value):
            value = value[int(token)]
        else:
            raise ToolError('CORPUS_MISSING_POINTER', 'Expected JSON pointer does not exist in engine output')
    return value


def evaluate(result: ProcessResult, expected: dict[str, Any]) -> list[str]:
    if result.termination:
        raise ToolError('CORPUS_PROCESS_LIMIT', 'Corpus command exceeded its execution budget')
    if result.returncode < 0 or result.returncode > 65535:
        raise ToolError('CORPUS_CRASH', 'Corpus command terminated abnormally')
    if result.returncode != expected['exit_code']:
        raise ToolError('CORPUS_EXIT_CODE', 'Corpus command returned an unexpected exit code')
    value = parse_json(result.stdout if result.returncode == 0 else result.stderr)
    if not isinstance(value, dict) or value.get('schema') != 'docsight.agent/v2':
        raise ToolError('CORPUS_PROTOCOL', 'Engine did not return the versioned agent envelope')
    if result.returncode == 0:
        if result.stderr or not isinstance(value.get('result'), dict):
            raise ToolError('CORPUS_PROTOCOL', 'Successful engine output is not a clean result envelope')
    elif (result.stdout or not isinstance(value.get('error'), dict)
          or value['error'].get('exit_code') != result.returncode):
        raise ToolError('CORPUS_PROTOCOL', 'Error output is not a clean typed error envelope')
    records = value.get('warnings', [])
    if not isinstance(records, list):
        raise ToolError('CORPUS_PROTOCOL', 'Warnings must be an array')
    records = records + ([value['error']] if isinstance(value.get('error'), dict) else [])
    codes = sorted({item['code'] for item in records if isinstance(item, dict) and isinstance(item.get('code'), str)})
    if not set(expected['diagnostic_codes']) <= set(codes):
        raise ToolError('CORPUS_DIAGNOSTIC', 'A required diagnostic is missing')
    for pointer, wanted in expected['pointer_equals'].items():
        actual = json_pointer(value, pointer)
        if type(actual) is not type(wanted) or actual != wanted:
            raise ToolError('CORPUS_ASSERTION', 'A reviewed result assertion failed')
    return [code for code in expected['diagnostic_codes'] if code in codes]


def run_corpus(archive: Path, manifest_path: Path, root: Path = ROOT,
               runner: Callable[..., ProcessResult] = run_bounded) -> dict[str, Any]:
    corpus = load_manifest(manifest_path, root)
    manifest_digest = sha256_file(manifest_path)
    manifest = verify_archive(archive)
    if manifest['target'] != native_target():
        raise ToolError('CORPUS_HOST_MISMATCH', 'Corpus execution requires a native release archive')
    outcomes = []
    with tempfile.TemporaryDirectory(prefix='docsight-corpus-') as directory:
        temporary = Path(directory)
        binary, manifest = extract_verified(archive, temporary / 'package')
        work = temporary / 'work'
        work.mkdir()
        environment = isolated_environment()
        version = runner([str(binary), '--version'], cwd=work, timeout=10, output_limit=4096, env=environment)
        if (version.termination or version.returncode != 0 or version.stderr
                or version.stdout.strip() != f"docsight {manifest['version']}".encode()):
            raise ToolError('CORPUS_VERSION_MISMATCH', 'Executable version differs from the archive manifest')
        for case in corpus['cases']:
            output = work / f"{case['id']}.png"
            document = (root / case['file']).resolve()
            arguments = [str(binary), '--agent', '--sandbox', case['operation'], str(document)]
            if case['operation'] == 'render':
                arguments += ['--page', '1', '--dpi', '72', '--out', str(output)]
            if case['operation'] == 'diff':
                arguments.append(str(document))
            attempts, elapsed, codes, digest, error_code = 0, 0, [], None, None
            try:
                for _ in range(case['expected']['repeat']):
                    if sha256_file(document) != case['sha256']:
                        raise ToolError('CORPUS_DIGEST_MISMATCH', 'Corpus input changed during the campaign')
                    output.unlink(missing_ok=True)
                    result = runner(arguments, cwd=work, timeout=45, output_limit=8_388_608, env=environment)
                    attempts += 1
                    elapsed += result.elapsed_ms
                    codes = evaluate(result, case['expected'])
                    digest_input = result.stdout + b'\x00' + result.stderr
                    if case['operation'] == 'render' and result.returncode == 0:
                        validate_png(output)
                        digest_input += bytes.fromhex(sha256_file(output))
                    current_digest = hashlib.sha256(digest_input).hexdigest()
                    if digest is not None and current_digest != digest:
                        raise ToolError('CORPUS_NONDETERMINISTIC', 'Repeated command changed output bytes')
                    digest = current_digest
            except (ToolError, OSError, ValueError) as error:
                error_code = error.code if isinstance(error, ToolError) else 'CORPUS_IO_ERROR'
            outcomes.append({'id': case['id'], 'origin': case['origin'], 'format': case['format'],
                             'document_sha256': case['sha256'], 'operation': case['operation'],
                             'passed': error_code is None, 'error_code': error_code, 'attempts': attempts,
                             'elapsed_ms': elapsed, 'output_sha256': digest, 'diagnostic_codes': codes})
    return {'schema': 'docsight.corpus-report/v1', 'version': manifest['version'],
            'revision': manifest['revision'], 'target': manifest['target'],
            'archive_sha256': sha256_file(archive), 'manifest_sha256': manifest_digest,
            'cases': outcomes, 'passed': all(case['passed'] for case in outcomes)}


def main() -> int:
    parser = argparse.ArgumentParser(description='Verify and execute reviewed, content-addressed regression cases')
    parser.add_argument('command', choices=('validate', 'run'))
    parser.add_argument('--manifest', type=Path, default=ROOT / 'release/corpus.json')
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--archive', type=Path)
    parser.add_argument('--out', type=Path)
    arguments = parser.parse_args()
    try:
        if arguments.command == 'validate':
            manifest = load_manifest(arguments.manifest, arguments.root)
            report = {'schema': 'docsight.corpus-inventory/v1', 'cases': len(manifest['cases']), 'executed': False,
                      'manifest_sha256': sha256_file(arguments.manifest)}
        else:
            if arguments.archive is None or arguments.out is None:
                raise ToolError('CORPUS_RUN_ARGUMENTS', 'Execution requires an archive and an output report')
            report = run_corpus(arguments.archive, arguments.manifest, arguments.root)
        if arguments.out is not None:
            write_new(arguments.out, json_bytes(report))
        sys.stdout.buffer.write(json_bytes(report))
        return 0 if report.get('passed', True) else 1
    except (ToolError, OSError, ValueError) as error:
        sys.stderr.buffer.write(error_json(error))
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
