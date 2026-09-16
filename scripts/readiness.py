from __future__ import annotations

import argparse
from pathlib import Path
import re
import sys
from typing import Any, Callable

from scripts.beta import aggregate_reports, validate_report
from scripts.common import ROOT, ToolError, bounded_integer, error_json, exact_keys, json_bytes, read_json, sha256_file, write_new
from scripts.corpus import load_manifest
from scripts.release import TARGETS, checked_revision, safe_member, validate_smoke_receipt, verify_archive, workspace_version
from scripts.validate import CHECK_NAMES

REVIEWS = ('policy', 'installation', 'behavior-and-json', 'render-and-diff',
           'security-and-fuzzing', 'beta-participation-and-triage')
CRITERIA = ('workspace-validation', 'five-native-packages', 'beta-observations', 'broad-corpus',
            'manual-reviews', 'beta-regressions', 'known-v1-gaps')


def bound_file(root: Path, reference: Any) -> Path:
    exact_keys(reference, {'file', 'sha256'}, 'evidence reference')
    relative = safe_member(reference['file'])
    path = root / relative
    if path.is_symlink() or not path.is_file() or not path.resolve().is_relative_to(root.resolve()):
        raise ToolError('INVALID_EVIDENCE_PATH', 'Evidence must be a regular contained file')
    if sha256_file(path) != reference['sha256']:
        raise ToolError('EVIDENCE_DIGEST_MISMATCH', 'Evidence changed after it was recorded')
    return path


def identity(value: Any, schema: str, revision: str, version: str) -> None:
    if (not isinstance(value, dict) or value.get('schema') != schema
            or value.get('revision') != revision or value.get('version') != version):
        raise ToolError('EVIDENCE_CANDIDATE_MISMATCH', 'Evidence does not describe the exact candidate and schema')


def load_policy(root: Path) -> dict[str, Any]:
    policy = read_json(root / 'release/readiness-policy.json')
    exact_keys(policy, {'schema', 'minimum_beta_participants', 'minimum_corpus_documents',
                        'minimum_real_documents_per_format', 'required_reviews'}, 'readiness policy')
    if policy['schema'] != 'docsight.readiness-policy/v1' or policy['required_reviews'] != list(REVIEWS):
        raise ToolError('INVALID_READINESS_POLICY', 'Readiness policy must preserve every review category')
    bounded_integer(policy['minimum_beta_participants'], 5, 999, 'minimum beta participants')
    bounded_integer(policy['minimum_corpus_documents'], 2, 100_000, 'minimum corpus documents')
    bounded_integer(policy['minimum_real_documents_per_format'], 1, 50_000, 'minimum real documents per format')
    return policy


def validate_workspace(directory: Path, revision: str, version: str) -> None:
    value = read_json(directory / 'validation/validation.json')
    identity(value, 'docsight.validation/v1', revision, version)
    for field in ('passed', 'clean_tree_before', 'clean_tree_after', 'same_revision_after'):
        if value.get(field) is not True:
            raise ToolError('INCOMPLETE_WORKSPACE_VALIDATION', 'All workspace gates must pass on the same clean revision')
    checks = value.get('checks')
    if (not isinstance(checks, list) or len(checks) != len(CHECK_NAMES)
            or [check.get('name') for check in checks if isinstance(check, dict)] != list(CHECK_NAMES)):
        raise ToolError('MISSING_VALIDATION_GATES', 'Workspace validation omitted or duplicated a required gate')
    for check in checks:
        if check.get('status') != 'passed' or type(check.get('exit_code')) is not int or check['exit_code'] != 0 or check.get('reason') is not None:
            raise ToolError('FAILED_VALIDATION_GATE', 'A required workspace gate did not pass')
        for stream in ('stdout', 'stderr'):
            bound_file(directory / 'validation', {'file': check.get(f'{stream}_log'), 'sha256': check.get(f'{stream}_sha256')})


def package_evidence(directory: Path, revision: str, version: str) -> dict[str, str]:
    archives = sorted(directory.glob('docsight-*.zip'))
    if len(archives) != len(TARGETS):
        raise ToolError('MISSING_NATIVE_PACKAGES', 'Readiness requires all five native package archives')
    found = {}
    toolchains = set()
    for archive in archives:
        manifest = verify_archive(archive)
        identity(manifest, 'docsight.release/v1', revision, version)
        if manifest['target'] in found:
            raise ToolError('DUPLICATE_NATIVE_PACKAGE', 'Each native target must occur exactly once')
        validate_smoke_receipt(read_json(directory / f"smoke-{manifest['target']}.json"), manifest, sha256_file(archive))
        found[manifest['target']] = sha256_file(archive)
        toolchains.add(manifest['toolchain'])
        if archive.with_name(archive.name + '.sha256').read_text() != f'{sha256_file(archive)}  {archive.name}\n':
            raise ToolError('ARCHIVE_CHECKSUM_MISMATCH', 'A native archive differs from its published checksum')
    if set(found) != set(TARGETS) or len(toolchains) != 1:
        raise ToolError('MISSING_NATIVE_PACKAGES', 'Readiness requires the exact native platform matrix')
    return found


def beta_evidence(directory: Path, packages: dict[str, str], revision: str, version: str, minimum: int) -> None:
    summary = aggregate_reports(directory / 'beta')
    identity(summary, 'docsight.beta-summary/v1', revision, version)
    if len(summary['participants']) < minimum:
        raise ToolError('INSUFFICIENT_BETA_PARTICIPANTS', 'The candidate lacks the required number of beta participants')
    for path in sorted((directory / 'beta').glob('*.json')):
        report = validate_report(read_json(path))
        if report['archive_sha256'] != packages.get(report['target']):
            raise ToolError('BETA_PACKAGE_MISMATCH', 'A beta observation did not use a verified candidate package')


def corpus_evidence(directory: Path, packages: dict[str, str], revision: str, version: str,
                    policy: dict[str, Any]) -> dict[str, dict[str, Any]]:
    manifest_path = directory / 'corpus-manifest.json'
    manifest = load_manifest(manifest_path, None)
    cases = {}
    documents, real = set(), {'docx': set(), 'pdf': set()}
    for case in manifest['cases']:
        cases[case['id']] = case
        if case['format'] in real and case['expected'].get('exit_code') == 0:
            documents.add(case['sha256'])
            if case['origin'] == 'consented-real':
                real[case['format']].add(case['sha256'])
    if (len(documents) < policy['minimum_corpus_documents']
            or any(len(values) < policy['minimum_real_documents_per_format'] for values in real.values())):
        raise ToolError('INSUFFICIENT_REAL_CORPUS', 'Synthetic samples and repeated cases do not satisfy the broad real-document corpus policy')
    for target in TARGETS:
        report = read_json(directory / f'corpus-{target}.json', 16_777_216)
        identity(report, 'docsight.corpus-report/v1', revision, version)
        if (report.get('target') != target or report.get('archive_sha256') != packages.get(target)
                or report.get('manifest_sha256') != sha256_file(manifest_path) or report.get('passed') is not True):
            raise ToolError('FAILED_CORPUS_CANDIDATE', 'Every native corpus report must pass for the exact archive and manifest')
        outcomes = report.get('cases')
        if not isinstance(outcomes, list) or len(outcomes) != len(cases):
            raise ToolError('MISSING_CORPUS_CASES', 'Corpus report omitted required cases')
        seen = set()
        for outcome in outcomes:
            if not isinstance(outcome, dict) or not isinstance(outcome.get('id'), str):
                raise ToolError('INVALID_CORPUS_OUTCOME', 'Corpus result must identify a reviewed case')
            case = cases.get(outcome['id'])
            if case is None or outcome['id'] in seen:
                raise ToolError('MISSING_CORPUS_CASES', 'Corpus cases cannot be substituted or duplicated')
            seen.add(outcome['id'])
            if (outcome.get('passed') is not True or outcome.get('error_code') is not None
                    or type(outcome.get('attempts')) is not int or outcome['attempts'] != case['expected']['repeat']
                    or outcome.get('document_sha256') != case['sha256']
                    or outcome.get('reference_sha256') != case.get('reference', {}).get('sha256')
                    or any(outcome.get(field) != case[field] for field in ('origin', 'format', 'operation'))):
                raise ToolError('FAILED_CORPUS_CASE', 'A corpus case lacks complete passing execution evidence')
    return cases


def review_evidence(directory: Path, revision: str, version: str, root: Path) -> None:
    value = read_json(directory / 'reviews.json')
    identity(value, 'docsight.release-reviews/v1', revision, version)
    if value.get('policy_sha256') != sha256_file(root / 'release/readiness-policy.json'):
        raise ToolError('UNREVIEWED_READINESS_POLICY', 'Reviewers must approve the exact acceptance thresholds')
    reviews = value.get('reviews')
    if not isinstance(reviews, dict) or set(reviews) != set(REVIEWS):
        raise ToolError('MISSING_MANUAL_REVIEWS', 'Every plan criterion requires a recorded independent review')
    for review in reviews.values():
        exact_keys(review, {'approved', 'reviewer', 'evidence'}, 'manual review')
        if review['approved'] is not True or not isinstance(review['reviewer'], str) or not re.fullmatch(r'[A-Za-z0-9_-]{3,80}', review['reviewer']):
            raise ToolError('PENDING_MANUAL_REVIEW', 'A named reviewer has not approved a required criterion')
        path = bound_file(directory, review['evidence'])
        if not 100 <= path.stat().st_size <= 1_048_576:
            raise ToolError('EMPTY_MANUAL_REVIEW', 'Review evidence must contain the procedure, observations and limitations')


def regression_evidence(directory: Path, cases: dict[str, dict[str, Any]], root: Path, revision: str) -> None:
    value = read_json(root / 'release/beta-issues.json')
    exact_keys(value, {'schema', 'issues'}, 'beta issue register')
    if value['schema'] != 'docsight.beta-issues/v1' or not isinstance(value['issues'], list):
        raise ToolError('INVALID_BETA_ISSUES', 'Beta issue register must be versioned')
    seen = set()
    for issue in value['issues']:
        exact_keys(issue, {'id', 'severity', 'status', 'regression_case', 'before_evidence'}, 'beta issue')
        if not isinstance(issue['id'], str) or issue['id'] in seen:
            raise ToolError('INVALID_BETA_ISSUES', 'Beta issue identifiers must be unique')
        seen.add(issue['id'])
        if issue['status'] not in ('open', 'resolved', 'deferred') or issue['severity'] not in ('blocker', 'high', 'medium', 'low'):
            raise ToolError('INVALID_BETA_ISSUES', 'Beta issue status and severity must be explicit')
        if issue['status'] != 'resolved':
            if issue['severity'] in ('blocker', 'high'):
                raise ToolError('OPEN_BETA_BLOCKER', 'A high-severity beta issue remains unresolved')
            continue
        if not isinstance(issue['regression_case'], str) or issue['regression_case'] not in cases:
            raise ToolError('MISSING_BETA_REGRESSION', 'Resolved beta issues require a passing regression case')
        before = read_json(bound_file(directory, issue['before_evidence']))
        if (not isinstance(before, dict) or before.get('schema') != 'docsight.corpus-report/v1'
                or before.get('revision') == revision or not isinstance(before.get('cases'), list)):
            raise ToolError('MISSING_PRE_FIX_FAILURE', 'Regression evidence must preserve a distinct pre-fix execution')
        checked_revision(before.get('revision'))
        failures = [case for case in before['cases'] if isinstance(case, dict) and case.get('id') == issue['regression_case']]
        if (len(failures) != 1 or failures[0].get('passed') is not False
                or type(failures[0].get('attempts')) is not int or failures[0]['attempts'] < 1
                or failures[0].get('document_sha256') != cases[issue['regression_case']]['sha256']
                or failures[0].get('reference_sha256') != cases[issue['regression_case']].get('reference', {}).get('sha256')
                or failures[0].get('error_code') not in ('CORPUS_ASSERTION', 'CORPUS_DIAGNOSTIC', 'CORPUS_CRASH',
                   'CORPUS_EXIT_CODE', 'CORPUS_NONDETERMINISTIC', 'CORPUS_PROCESS_LIMIT', 'SMOKE_INVALID_PNG')):
            raise ToolError('MISSING_PRE_FIX_FAILURE', 'Resolved beta issue must have a reproduced failure before its fix')


def known_gaps(root: Path) -> None:
    value = read_json(root / 'release/known-gaps.json')
    exact_keys(value, {'schema', 'items'}, 'known gap register')
    if value['schema'] != 'docsight.known-gaps/v1' or not isinstance(value['items'], list):
        raise ToolError('INVALID_KNOWN_GAPS', 'Known product gaps must remain visible and versioned')
    seen = set()
    for item in value['items']:
        exact_keys(item, {'id', 'blocking', 'status', 'source'}, 'known gap')
        if (not isinstance(item['id'], str) or item['id'] in seen or type(item['blocking']) is not bool
                or item['status'] not in ('open', 'resolved', 'deferred-until-real-case')):
            raise ToolError('INVALID_KNOWN_GAPS', 'Known gap identifiers and state must be explicit')
        seen.add(item['id'])
        source = root / safe_member(item['source'])
        if not source.is_file() or not source.resolve().is_relative_to(root.resolve()):
            raise ToolError('INVALID_KNOWN_GAPS', 'Known gaps must reference existing project documentation')
        if item['blocking'] and item['status'] != 'resolved':
            raise ToolError('OPEN_V1_PRODUCT_GAPS', 'The project still declares unresolved v1-blocking engine gaps')


def assess(directory: Path, revision: str, root: Path = ROOT) -> dict[str, Any]:
    checked_revision(revision)
    version, policy = workspace_version(root), load_policy(root)
    criteria = []
    packages: dict[str, str] = {}
    cases: dict[str, dict[str, Any]] = {}

    def check(name: str, operation: Callable[[], Any]) -> Any:
        try:
            result = operation()
            criteria.append({'name': name, 'passed': True, 'error_code': None})
            return result
        except (ToolError, OSError, ValueError, TypeError, KeyError) as error:
            code = error.code if isinstance(error, ToolError) else 'EVIDENCE_UNAVAILABLE_OR_INVALID'
            criteria.append({'name': name, 'passed': False, 'error_code': code})
            return None

    check('workspace-validation', lambda: validate_workspace(directory, revision, version))
    packages = check('five-native-packages', lambda: package_evidence(directory, revision, version)) or {}
    check('beta-observations', lambda: beta_evidence(directory, packages, revision, version, policy['minimum_beta_participants']))
    cases = check('broad-corpus', lambda: corpus_evidence(directory, packages, revision, version, policy)) or {}
    check('manual-reviews', lambda: review_evidence(directory, revision, version, root))
    check('beta-regressions', lambda: regression_evidence(directory, cases, root, revision))
    check('known-v1-gaps', lambda: known_gaps(root))
    return {'schema': 'docsight.readiness/v1', 'version': version, 'revision': revision,
            'policy_sha256': sha256_file(root / 'release/readiness-policy.json'), 'criteria': criteria,
            'ready_for_v1': all(item['passed'] for item in criteria),
            'scope': 'artifact consistency plus explicitly recorded human reviews; not independent certification'}


def main() -> int:
    parser = argparse.ArgumentParser(description='Fail closed unless every v1 acceptance criterion has candidate-bound evidence')
    parser.add_argument('--evidence', type=Path, required=True)
    parser.add_argument('--revision', required=True)
    parser.add_argument('--out', type=Path, required=True)
    arguments = parser.parse_args()
    try:
        report = assess(arguments.evidence, arguments.revision)
        write_new(arguments.out, json_bytes(report))
        sys.stdout.buffer.write(json_bytes(report))
        return 0 if report['ready_for_v1'] else 1
    except (ToolError, OSError, ValueError) as error:
        sys.stderr.buffer.write(error_json(error))
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
