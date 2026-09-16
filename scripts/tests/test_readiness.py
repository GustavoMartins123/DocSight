import copy
from pathlib import Path
import unittest

from scripts.beta import summarize_result
from scripts.common import ROOT, ProcessResult, ToolError, json_bytes, read_json, sha256_file
from scripts.readiness import CRITERIA, REVIEWS, assess, known_gaps
from scripts.release import TARGETS
from scripts.tests.test_release import ReleaseFixture
from scripts.validate import run_validation


class ReadinessTests(ReleaseFixture):
    def setUp(self):
        super().setUp()
        (self.root / 'release').mkdir()
        self.policy = {'schema': 'docsight.readiness-policy/v1', 'minimum_beta_participants': 5,
                       'minimum_corpus_documents': 2, 'minimum_real_documents_per_format': 1,
                       'required_reviews': list(REVIEWS)}
        self.save(self.root / 'release/readiness-policy.json', self.policy)
        self.save(self.root / 'release/known-gaps.json', {'schema': 'docsight.known-gaps/v1', 'items': []})
        self.save(self.root / 'release/beta-issues.json', {'schema': 'docsight.beta-issues/v1', 'issues': []})
        self.evidence = self.root / 'evidence'
        self.archives = {}
        for target in TARGETS:
            archive = self.package(target, out='evidence')
            self.receipt(archive)
            self.archives[target] = sha256_file(archive)
        run_validation(self.evidence / 'validation', self.root, self.validation_runner)
        (self.evidence / 'beta').mkdir()
        for number in range(1, 6):
            target = 'x86_64-unknown-linux-gnu'
            report = summarize_result(ProcessResult(0, json_bytes({'schema': 'docsight.agent/v2', 'result': {}}), b'', number),
                manifest={'version': '0.1.4', 'revision': self.revision, 'target': target}, archive_digest=self.archives[target],
                participant=f'beta-{number:03}', operation='inspect', experience='clear', document_size=100,
                document_digest=None, allowed=set())
            self.save(self.evidence / f'beta/{number}.json', report)
        self.cases = [{'id': f'synthetic-evidence-{kind}', 'file': f'synthetic-evidence/{kind}.{kind}',
                       'sha256': digest * 64, 'origin': 'consented-real', 'format': kind, 'operation': 'inspect',
                       'expected': {'repeat': 2, 'exit_code': 0, 'diagnostic_codes': [], 'pointer_equals': {'/result/format': kind}}}
                      for kind, digest in (('docx', 'c'), ('pdf', 'd'))]
        self.save(self.evidence / 'corpus-manifest.json', {'schema': 'docsight.corpus/v1', 'cases': self.cases})
        for target in TARGETS:
            outcomes = [{'id': case['id'], 'origin': case['origin'], 'format': case['format'], 'operation': case['operation'],
                         'document_sha256': case['sha256'], 'passed': True, 'error_code': None, 'attempts': 2,
                         'elapsed_ms': 10, 'output_sha256': 'e' * 64, 'diagnostic_codes': []} for case in self.cases]
            self.save(self.evidence / f'corpus-{target}.json', {'schema': 'docsight.corpus-report/v1', 'version': '0.1.4',
                      'revision': self.revision, 'target': target, 'archive_sha256': self.archives[target],
                      'manifest_sha256': sha256_file(self.evidence / 'corpus-manifest.json'), 'passed': True, 'cases': outcomes})
        review = self.evidence / 'synthetic-unit-test-review.md'
        review.write_text('This is fabricated evidence used only by a consistency-gate unit test, not an actual human acceptance or product certification.\n')
        self.save(self.evidence / 'reviews.json', {'schema': 'docsight.release-reviews/v1', 'version': '0.1.4',
                  'revision': self.revision, 'policy_sha256': sha256_file(self.root / 'release/readiness-policy.json'),
                  'reviews': {name: {'approved': True, 'reviewer': 'synthetic-test-reviewer',
                                    'evidence': {'file': review.name, 'sha256': sha256_file(review)}} for name in REVIEWS}})

    def save(self, path, data):
        path.write_bytes(json_bytes(data))

    def validation_runner(self, arguments, **options):
        if arguments[:3] == ['git', 'rev-parse', 'HEAD']:
            return ProcessResult(0, (self.revision + '\n').encode(), b'', 1)
        return ProcessResult(0, b'', b'', 1)

    def status(self):
        return assess(self.evidence, self.revision, self.root)

    def criterion(self, name):
        return next(item for item in self.status()['criteria'] if item['name'] == name)

    def test_synthetic_complete_evidence_satisfies_the_consistency_gate(self):
        report = self.status()
        self.assertTrue(report['ready_for_v1'], report)
        self.assertEqual([item['name'] for item in report['criteria']], list(CRITERIA))
        self.assertIn('not independent certification', report['scope'])

    def test_missing_evidence_reports_all_criteria_without_false_acceptance(self):
        report = assess(self.root / 'absent', self.revision, self.root)
        self.assertFalse(report['ready_for_v1'])
        self.assertEqual(len(report['criteria']), len(CRITERIA))

    def test_a_passed_boolean_cannot_hide_a_blocked_rust_test(self):
        path = self.evidence / 'validation/validation.json'
        value = read_json(path)
        value['checks'][5].update(status='blocked', reason='executable_missing', exit_code=127)
        self.save(path, value)
        self.assertEqual(self.criterion('workspace-validation')['error_code'], 'FAILED_VALIDATION_GATE')
        self.assertFalse(self.status()['ready_for_v1'])

    def test_missing_and_duplicated_validation_checks_are_rejected(self):
        path = self.evidence / 'validation/validation.json'
        value = read_json(path)
        value['checks'][-1] = copy.deepcopy(value['checks'][0])
        self.save(path, value)
        self.assertEqual(self.criterion('workspace-validation')['error_code'], 'MISSING_VALIDATION_GATES')

    def test_validation_log_tampering_is_detected(self):
        (self.evidence / 'validation/cargo_test.stdout.log').write_text('replaced')
        self.assertEqual(self.criterion('workspace-validation')['error_code'], 'EVIDENCE_DIGEST_MISMATCH')

    def test_all_native_targets_are_required(self):
        next(self.evidence.glob('docsight-*.zip')).unlink()
        self.assertEqual(self.criterion('five-native-packages')['error_code'], 'MISSING_NATIVE_PACKAGES')

    def test_smoke_receipts_must_match_real_archive_digests(self):
        path = self.evidence / 'smoke-x86_64-unknown-linux-gnu.json'
        value = read_json(path)
        value['archive_sha256'] = '0' * 64
        self.save(path, value)
        self.assertFalse(self.criterion('five-native-packages')['passed'])

    def test_four_pseudonyms_do_not_satisfy_the_five_person_pilot(self):
        (self.evidence / 'beta/5.json').unlink()
        self.assertEqual(self.criterion('beta-observations')['error_code'], 'INSUFFICIENT_BETA_PARTICIPANTS')

    def test_beta_cannot_reference_an_unverified_binary(self):
        path = self.evidence / 'beta/1.json'
        value = read_json(path)
        value['archive_sha256'] = '0' * 64
        self.save(path, value)
        self.assertEqual(self.criterion('beta-observations')['error_code'], 'BETA_PACKAGE_MISMATCH')

    def test_synthetic_corpus_cannot_be_counted_as_real(self):
        value = read_json(self.evidence / 'corpus-manifest.json')
        for case in value['cases']:
            case['origin'] = 'synthetic'
        self.save(self.evidence / 'corpus-manifest.json', value)
        self.assertEqual(self.criterion('broad-corpus')['error_code'], 'INSUFFICIENT_REAL_CORPUS')

    def test_repeated_cases_do_not_inflate_unique_document_count(self):
        value = read_json(self.evidence / 'corpus-manifest.json')
        value['cases'][1]['sha256'] = value['cases'][0]['sha256']
        self.save(self.evidence / 'corpus-manifest.json', value)
        self.assertEqual(self.criterion('broad-corpus')['error_code'], 'INSUFFICIENT_REAL_CORPUS')

    def test_a_failed_case_cannot_be_hidden_by_passed_summary(self):
        path = self.evidence / 'corpus-x86_64-unknown-linux-gnu.json'
        value = read_json(path)
        value['cases'][0].update(passed=False, error_code='CORPUS_CRASH')
        self.save(path, value)
        self.assertEqual(self.criterion('broad-corpus')['error_code'], 'FAILED_CORPUS_CASE')

    def test_missing_repeated_executions_are_rejected(self):
        path = self.evidence / 'corpus-aarch64-unknown-linux-gnu.json'
        value = read_json(path)
        value['cases'][0]['attempts'] = 1
        self.save(path, value)
        self.assertEqual(self.criterion('broad-corpus')['error_code'], 'FAILED_CORPUS_CASE')

    def test_approval_must_cover_exact_reviewed_policy(self):
        self.policy['minimum_corpus_documents'] = 3
        self.save(self.root / 'release/readiness-policy.json', self.policy)
        self.assertEqual(self.criterion('manual-reviews')['error_code'], 'UNREVIEWED_READINESS_POLICY')

    def test_missing_human_review_blocks_acceptance(self):
        path = self.evidence / 'reviews.json'
        value = read_json(path)
        value['reviews']['security-and-fuzzing']['approved'] = False
        self.save(path, value)
        self.assertEqual(self.criterion('manual-reviews')['error_code'], 'PENDING_MANUAL_REVIEW')

    def test_known_engine_gap_cannot_be_hidden_by_passing_artifacts(self):
        self.save(self.root / 'release/known-gaps.json', {'schema': 'docsight.known-gaps/v1',
                  'items': [{'id': 'sections', 'blocking': True, 'status': 'open', 'source': 'README.md'}]})
        self.assertEqual(self.criterion('known-v1-gaps')['error_code'], 'OPEN_V1_PRODUCT_GAPS')
        self.assertFalse(self.status()['ready_for_v1'])

    def test_actual_repository_does_not_claim_its_known_v1_gaps_are_resolved(self):
        with self.assertRaisesRegex(ToolError, 'unresolved'):
            known_gaps(ROOT)

    def test_resolved_beta_bug_requires_a_reproduced_failure_and_passing_case(self):
        issue = {'id': 'BETA-001', 'status': 'resolved', 'severity': 'high',
                 'regression_case': self.cases[0]['id'], 'before_evidence': {'file': 'before.json', 'sha256': '0' * 64}}
        self.save(self.root / 'release/beta-issues.json', {'schema': 'docsight.beta-issues/v1', 'issues': [issue]})
        self.assertFalse(self.criterion('beta-regressions')['passed'])
        before = {'schema': 'docsight.corpus-report/v1', 'revision': 'b' * 40,
                  'cases': [{'id': self.cases[0]['id'], 'document_sha256': self.cases[0]['sha256'],
                             'passed': False, 'attempts': 1, 'error_code': 'CORPUS_ASSERTION'}]}
        self.save(self.evidence / 'before.json', before)
        issue['before_evidence']['sha256'] = sha256_file(self.evidence / 'before.json')
        self.save(self.root / 'release/beta-issues.json', {'schema': 'docsight.beta-issues/v1', 'issues': [issue]})
        self.assertTrue(self.criterion('beta-regressions')['passed'])
        before['cases'][0]['error_code'] = 'CORPUS_IO_ERROR'
        self.save(self.evidence / 'before.json', before)
        issue['before_evidence']['sha256'] = sha256_file(self.evidence / 'before.json')
        self.save(self.root / 'release/beta-issues.json', {'schema': 'docsight.beta-issues/v1', 'issues': [issue]})
        self.assertEqual(self.criterion('beta-regressions')['error_code'], 'MISSING_PRE_FIX_FAILURE')


if __name__ == '__main__':
    unittest.main()
