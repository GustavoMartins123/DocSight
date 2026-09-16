from __future__ import annotations

import argparse
from collections import Counter
import math
import hashlib
from pathlib import Path
import re
import statistics
import sys
import tempfile
from typing import Any, Callable

from scripts.common import (
    ROOT, ProcessResult, ToolError, bounded_integer, error_json, exact_keys,
    isolated_environment, json_bytes, parse_json, read_json, run_bounded,
    sha256_file, write_new,
)
from scripts.release import TARGETS, checked_revision, checked_version, verify_archive
from scripts.smoke import extract_verified, native_target

OPERATIONS = ('capabilities', 'inspect', 'overview', 'text', 'render', 'diff')
EXPERIENCES = ('clear', 'confusing', 'blocked')
OUTCOMES = ('success', 'error', 'crash', 'timeout', 'output_limit', 'invalid_protocol')
REPORT_FIELDS = {
    'schema', 'version', 'revision', 'target', 'archive_sha256', 'participant',
    'operation', 'experience', 'outcome', 'exit_code', 'elapsed_ms', 'stdout_bytes',
    'stderr_bytes', 'document_size_bytes', 'document_sha256', 'diagnostic_codes',
    'unknown_diagnostic_count',
}


def _pseudonym(value: Any) -> str:
    if not isinstance(value, str) or re.fullmatch(r'beta-[0-9]{3}', value) is None:
        raise ToolError('INVALID_PARTICIPANT', 'Use an anonymous participant identifier such as beta-001')
    return value


def known_diagnostics(root: Path = ROOT) -> set[str]:
    documentation = (root / 'PRODUCT_SCOPE.md').read_text() + (root / 'AGENT_PROTOCOL.md').read_text()
    return set(re.findall(r'\b[A-Z][A-Z0-9_]{2,79}\b', documentation))


def _digest(value: Any, nullable: bool = False) -> None:
    if nullable and value is None:
        return
    if not isinstance(value, str) or re.fullmatch(r'[0-9a-f]{64}', value) is None:
        raise ToolError('INVALID_DIGEST', 'Report digests must be lowercase SHA-256 values')


def validate_report(report: Any, allowed: set[str] | None = None) -> dict[str, Any]:
    exact_keys(report, REPORT_FIELDS, 'beta report')
    if report['schema'] != 'docsight.beta-report/v1':
        raise ToolError('INVALID_BETA_SCHEMA', 'Beta report schema is not supported')
    checked_version(report['version'])
    checked_revision(report['revision'])
    if not isinstance(report['target'], str) or report['target'] not in TARGETS:
        raise ToolError('INVALID_BETA_TARGET', 'Beta report target is not supported')
    _pseudonym(report['participant'])
    for key, choices in (('operation', OPERATIONS), ('experience', EXPERIENCES), ('outcome', OUTCOMES)):
        if report[key] not in choices:
            raise ToolError('INVALID_BETA_VALUE', f'{key} is not one of the supported report values')
    _digest(report['archive_sha256'])
    _digest(report['document_sha256'], True)
    bounded_integer(report['exit_code'], -(2**31), 2**32 - 1, 'exit code')
    bounded_integer(report['elapsed_ms'], 0, 3_600_000, 'elapsed duration')
    for key in ('stdout_bytes', 'stderr_bytes', 'document_size_bytes'):
        bounded_integer(report[key], 0, 2**63 - 1, key)
    bounded_integer(report['unknown_diagnostic_count'], 0, 100_000, 'unknown diagnostic count')
    codes = report['diagnostic_codes']
    allowed = known_diagnostics() if allowed is None else allowed
    if (not isinstance(codes, list) or not all(isinstance(code, str) and code in allowed for code in codes)
            or len(codes) > 512 or codes != sorted(set(codes))):
        raise ToolError('INVALID_DIAGNOSTIC_CODES', 'Only sorted documented diagnostic codes may be shared')
    code, outcome = report['exit_code'], report['outcome']
    abnormal_exit = code < 0 or code > 65535
    if ((outcome == 'success' and code != 0)
            or (outcome == 'error' and not 1 <= code <= 65535)
            or (outcome == 'crash' and not abnormal_exit)
            or (outcome == 'invalid_protocol' and abnormal_exit)):
        raise ToolError('INCONSISTENT_BETA_OUTCOME', 'Beta outcome conflicts with the process exit code')
    return report


def _project_diagnostics(result: ProcessResult, allowed: set[str]) -> tuple[list[str], int, bool]:
    source = result.stdout if result.returncode == 0 else result.stderr
    try:
        envelope = parse_json(source)
    except ToolError:
        return [], 0, False
    if not isinstance(envelope, dict):
        return [], 0, False
    records = []
    warnings = envelope.get('warnings')
    if isinstance(warnings, list):
        records.extend(warnings)
    error = envelope.get('error')
    if isinstance(error, dict):
        records.append(error)
    if isinstance(envelope.get('code'), str):
        records.append(envelope)
    codes = set()
    unknown = 0
    for record in records:
        if not isinstance(record, dict) or not isinstance(record.get('code'), str):
            unknown += 1
        elif record['code'] in allowed:
            codes.add(record['code'])
        else:
            unknown += 1
    canonical = envelope.get('schema') == 'docsight.agent/v2'
    if result.returncode == 0:
        canonical = canonical and isinstance(envelope.get('result'), dict) and not result.stderr
    else:
        canonical = canonical and isinstance(error, dict) and error.get('exit_code') == result.returncode and not result.stdout
    return sorted(codes), unknown, canonical


def summarize_result(result: ProcessResult, *, manifest: dict[str, Any], archive_digest: str,
                     participant: str, operation: str, experience: str, document_size: int,
                     document_digest: str | None, allowed: set[str]) -> dict[str, Any]:
    codes, unknown, protocol_ok = _project_diagnostics(result, allowed)
    if result.termination is not None:
        outcome = result.termination
    elif result.returncode < 0 or result.returncode > 65535:
        outcome = 'crash'
    elif not protocol_ok:
        outcome = 'invalid_protocol'
    else:
        outcome = 'success' if result.returncode == 0 else 'error'
    report = {
        'schema': 'docsight.beta-report/v1', 'version': manifest['version'],
        'revision': manifest['revision'], 'target': manifest['target'],
        'archive_sha256': archive_digest, 'participant': participant,
        'operation': operation, 'experience': experience, 'outcome': outcome,
        'exit_code': result.returncode, 'elapsed_ms': result.elapsed_ms,
        'stdout_bytes': len(result.stdout), 'stderr_bytes': len(result.stderr),
        'document_size_bytes': document_size, 'document_sha256': document_digest,
        'diagnostic_codes': codes, 'unknown_diagnostic_count': unknown,
    }
    return validate_report(report, allowed)


def collect_report(archive: Path, participant: str, operation: str, experience: str,
                   document: Path | None = None, reference: Path | None = None,
                   password_file: Path | None = None, include_digest: bool = False,
                   runner: Callable[..., ProcessResult] = run_bounded) -> dict[str, Any]:
    _pseudonym(participant)
    if operation not in OPERATIONS or experience not in EXPERIENCES:
        raise ToolError('INVALID_BETA_OPERATION', 'Choose a documented operation and experience value')
    if operation != 'capabilities' and document is None:
        raise ToolError('BETA_DOCUMENT_REQUIRED', 'This operation requires an explicitly selected document')
    if (operation == 'diff') != (reference is not None):
        raise ToolError('BETA_REFERENCE_REQUIRED', 'Only diff requires a reference document')
    if operation == 'capabilities' and (document is not None or password_file is not None or include_digest):
        raise ToolError('UNUSED_BETA_INPUT', 'Capabilities does not consume document or password inputs')
    document = document.resolve(strict=True) if document is not None else None
    reference = reference.resolve(strict=True) if reference is not None else None
    password_file = password_file.resolve(strict=True) if password_file is not None else None
    for path in (document, reference, password_file):
        if path is not None and not path.is_file():
            raise ToolError('INVALID_BETA_INPUT', 'Selected inputs must be regular files')
    manifest = verify_archive(archive)
    if manifest['target'] != native_target():
        raise ToolError('BETA_HOST_MISMATCH', 'Beta collection requires a native archive')
    with tempfile.TemporaryDirectory(prefix='docsight-beta-') as directory:
        root = Path(directory)
        binary, manifest = extract_verified(archive, root / 'package')
        work = root / 'work'
        work.mkdir()
        environment = isolated_environment()
        version = runner([str(binary), '--version'], cwd=work, timeout=10, output_limit=4096, env=environment)
        if version.termination or version.returncode != 0 or version.stderr or version.stdout.strip() != f"docsight {manifest['version']}".encode():
            raise ToolError('BETA_VERSION_MISMATCH', 'Executable version differs from its release manifest')
        arguments = [str(binary), '--agent', '--sandbox', '--max-bytes', '65536']
        if password_file is not None:
            arguments += ['--password-file', str(password_file)]
        arguments.append(operation)
        if document is not None:
            arguments.append(str(document))
        if reference is not None:
            arguments.append(str(reference))
        if operation == 'render':
            arguments += ['--page', '1', '--dpi', '72', '--out', str(work / 'render.png')]
        result = runner(arguments, cwd=work, timeout=45, output_limit=262_144, env=environment)
        allowed = known_diagnostics(root / 'package')
    return summarize_result(result, manifest=manifest, archive_digest=sha256_file(archive),
                            participant=participant, operation=operation, experience=experience,
                            document_size=document.stat().st_size if document is not None else 0,
                            document_digest=sha256_file(document) if document is not None and include_digest else None,
                            allowed=allowed)


def aggregate_reports(directory: Path) -> dict[str, Any]:
    paths = sorted(directory.glob('*.json'))
    if not paths:
        raise ToolError('NO_BETA_REPORTS', 'No beta observations were collected for this campaign')
    bounded_integer(len(paths), 1, 10_000, 'beta report count')
    reports = [validate_report(read_json(path, 262_144)) for path in paths]
    digests = [sha256_file(path) for path in paths]
    canonical_digests = [hashlib.sha256(json_bytes(report)).hexdigest() for report in reports]
    if len(set(canonical_digests)) != len(canonical_digests):
        raise ToolError('DUPLICATE_BETA_REPORT', 'Copied reports cannot count as additional beta observations')
    identities = {(report['version'], report['revision']) for report in reports}
    if len(identities) != 1:
        raise ToolError('MIXED_BETA_CANDIDATES', 'Aggregate only reports for the same candidate version and revision')
    version, revision = identities.pop()
    performance = {}
    for operation in OPERATIONS:
        samples = sorted(report['elapsed_ms'] for report in reports if report['operation'] == operation)
        if samples:
            performance[operation] = {'samples': len(samples), 'p50_ms': statistics.median(samples),
                                      'p95_ms': samples[math.ceil(0.95 * len(samples)) - 1], 'maximum_ms': samples[-1]}
    return {
        'schema': 'docsight.beta-summary/v1', 'version': version, 'revision': revision,
        'participants': sorted({report['participant'] for report in reports}),
        'reports': len(reports), 'report_sha256s': sorted(digests),
        'archive_sha256s': sorted({report['archive_sha256'] for report in reports}),
        'operations': dict(sorted(Counter(report['operation'] for report in reports).items())),
        'outcomes': dict(sorted(Counter(report['outcome'] for report in reports).items())),
        'experience': dict(sorted(Counter(report['experience'] for report in reports).items())),
        'diagnostics': dict(sorted(Counter(code for report in reports for code in report['diagnostic_codes']).items())),
        'performance': performance,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description='Collect opt-in, local-only beta diagnostics without document content')
    commands = parser.add_subparsers(dest='command', required=True)
    collect = commands.add_parser('collect')
    collect.add_argument('--archive', type=Path, required=True)
    collect.add_argument('--participant', required=True)
    collect.add_argument('--operation', choices=OPERATIONS, required=True)
    collect.add_argument('--experience', choices=EXPERIENCES, required=True)
    collect.add_argument('--document', type=Path)
    collect.add_argument('--reference', type=Path)
    collect.add_argument('--password-file', type=Path)
    collect.add_argument('--include-document-digest', action='store_true')
    collect.add_argument('--out', type=Path, required=True)
    aggregate = commands.add_parser('aggregate')
    aggregate.add_argument('--reports', type=Path, required=True)
    aggregate.add_argument('--out', type=Path, required=True)
    arguments = parser.parse_args()
    try:
        if arguments.command == 'collect':
            report = collect_report(arguments.archive, arguments.participant, arguments.operation,
                                    arguments.experience, arguments.document, arguments.reference,
                                    arguments.password_file, arguments.include_document_digest)
        else:
            report = aggregate_reports(arguments.reports)
        write_new(arguments.out, json_bytes(report))
        sys.stdout.buffer.write(json_bytes(report))
        return 0
    except (ToolError, OSError, ValueError) as error:
        sys.stderr.buffer.write(error_json(error))
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
