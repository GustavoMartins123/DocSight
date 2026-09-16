import copy
import json
from pathlib import Path
import unittest
from unittest.mock import patch

from scripts.beta import aggregate_reports, collect_report, summarize_result, validate_report
from scripts.common import ProcessResult, ToolError, json_bytes
from scripts.tests.test_release import ReleaseFixture


class BetaTests(ReleaseFixture):
    def setUp(self):
        super().setUp()
        (self.root / 'PRODUCT_SCOPE.md').write_text('UNSUPPORTED_FORMAT PDF_PASSWORD_REQUIRED\n')
        self.allowed = {'UNSUPPORTED_FORMAT', 'PDF_PASSWORD_REQUIRED'}
        self.manifest = {'version': '0.1.4', 'revision': self.revision, 'target': 'x86_64-unknown-linux-gnu'}
        self.calls = []
        self.result = ProcessResult(0, json_bytes({'schema': 'docsight.agent/v2', 'result': {'text': 'private text'}}), b'', 123)

    def summarize(self, result=None, **options):
        values = dict(manifest=self.manifest, archive_digest='b' * 64, participant='beta-001',
                      operation='inspect', experience='clear', document_size=500,
                      document_digest=None, allowed=self.allowed)
        values.update(options)
        return summarize_result(result or self.result, **values)

    def engine(self, arguments, **options):
        self.calls.append((arguments, options))
        if '--version' in arguments:
            return ProcessResult(0, b'docsight 0.1.4\n', b'', 1)
        return self.result

    def test_projection_never_copies_document_text_or_messages(self):
        secret = 'patient Alice private-token /private/customer.pdf'
        result = ProcessResult(10, b'', json_bytes({'schema': 'docsight.agent/v2',
                               'error': {'code': 'UNSUPPORTED_FORMAT', 'message': secret, 'exit_code': 10},
                               'warnings': [{'code': secret, 'message': secret}]}), 32)
        report = self.summarize(result)
        self.assertNotIn(secret, json.dumps(report))
        self.assertEqual(report['diagnostic_codes'], ['UNSUPPORTED_FORMAT'])
        self.assertEqual(report['unknown_diagnostic_count'], 1)
        self.assertEqual(report['outcome'], 'error')
        self.assertIsNone(report['document_sha256'])

    def test_success_keeps_only_metadata(self):
        report = self.summarize()
        self.assertEqual(report['outcome'], 'success')
        self.assertNotIn('private text', json.dumps(report))
        self.assertEqual(report['elapsed_ms'], 123)

    def test_crash_timeout_and_output_limit_remain_distinct(self):
        for expected, result in (
            ('crash', ProcessResult(-11, b'', b'secret crash', 2)),
            ('crash', ProcessResult(3221225477, b'', b'', 2)),
            ('timeout', ProcessResult(-9, b'', b'', 45000, 'timeout')),
            ('output_limit', ProcessResult(0, b'', b'', 3, 'output_limit')),
        ):
            with self.subTest(expected=expected):
                self.assertEqual(self.summarize(result)['outcome'], expected)

    def test_zero_exit_alone_cannot_be_success(self):
        for output in (b'not JSON', b'{}', b'{"schema":"docsight.agent/v2"}'):
            self.assertEqual(self.summarize(ProcessResult(0, output, b'', 1))['outcome'], 'invalid_protocol')
        self.assertEqual(self.summarize(ProcessResult(0, self.result.stdout, b'warning', 1))['outcome'], 'invalid_protocol')

    def test_error_exit_must_match_versioned_envelope(self):
        result = ProcessResult(10, b'', json_bytes({'error': {'code': 'UNSUPPORTED_FORMAT', 'exit_code': 20}}), 1)
        self.assertEqual(self.summarize(result)['outcome'], 'invalid_protocol')

    def test_unexpected_fields_and_personal_identifiers_are_rejected(self):
        for field, value in (('participant', 'alice@example.com'), ('document_sha256', 'filename.docx'),
                             ('diagnostic_codes', ['SECRET']), ('elapsed_ms', True), ('outcome', 'passed')):
            report = self.summarize()
            report[field] = value
            with self.subTest(field=field), self.assertRaises(ToolError):
                validate_report(report, self.allowed)
        report = self.summarize()
        report['document_path'] = '/private/a.pdf'
        with self.assertRaises(ToolError):
            validate_report(report, self.allowed)

    def test_collect_is_opt_in_sandboxed_and_password_is_not_persisted(self):
        document = self.root / 'patient-A.docx'
        document.write_bytes(b'private document')
        password = self.root / 'secret-password.txt'
        password.write_text('super secret password')
        with patch('scripts.beta.native_target', return_value=self.manifest['target']):
            report = collect_report(self.package(), 'beta-002', 'render', 'confusing', document,
                                    password_file=password, runner=self.engine)
        self.assertEqual(report['experience'], 'confusing')
        self.assertIsNone(report['document_sha256'])
        data = json.dumps(report)
        self.assertNotIn('patient-A', data)
        self.assertNotIn('secret-password', data)
        self.assertNotIn('private document', data)
        arguments, options = self.calls[-1]
        self.assertIn('--sandbox', arguments)
        self.assertIn('--password-file', arguments)
        self.assertEqual(options['output_limit'], 262144)
        self.assertFalse(Path(arguments[arguments.index('--out') + 1]).parent.exists())

    def test_document_fingerprint_requires_explicit_opt_in(self):
        document = self.root / 'sample.pdf'
        document.write_bytes(b'abc')
        with patch('scripts.beta.native_target', return_value=self.manifest['target']):
            report = collect_report(self.package(), 'beta-001', 'inspect', 'clear', document,
                                    include_digest=True, runner=self.engine)
        self.assertEqual(report['document_sha256'], 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad')

    def test_document_and_reference_are_required_explicitly(self):
        for operation, document, reference in (('inspect', None, None), ('diff', self.root, None),
                                                ('capabilities', self.root, None), ('inspect', self.root, self.root)):
            with self.subTest(operation=operation), self.assertRaises(ToolError):
                collect_report(self.root / 'missing.zip', 'beta-001', operation, 'clear', document, reference,
                               runner=self.engine)
        self.assertEqual(self.calls, [])

    def test_wrong_host_does_not_run_binary(self):
        with patch('scripts.beta.native_target', return_value='aarch64-apple-darwin'):
            with self.assertRaises(ToolError):
                collect_report(self.package(), 'beta-001', 'capabilities', 'clear', runner=self.engine)
        self.assertEqual(self.calls, [])

    def reports(self):
        directory = self.root / 'reports'
        directory.mkdir()
        for index, elapsed in enumerate((10, 20, 30, 40, 50)):
            report = self.summarize(participant=f'beta-{index + 1:03}')
            report['elapsed_ms'] = elapsed
            (directory / f'{index}.json').write_bytes(json_bytes(report))
        return directory

    def test_aggregate_counts_unique_pseudonyms_and_actual_observations(self):
        summary = aggregate_reports(self.reports())
        self.assertEqual(len(summary['participants']), 5)
        self.assertEqual(summary['reports'], 5)
        self.assertEqual(summary['outcomes'], {'success': 5})
        self.assertEqual(summary['performance']['inspect'], {'samples': 5, 'p50_ms': 30, 'p95_ms': 50, 'maximum_ms': 50})

    def test_same_participant_does_not_inflate_participant_count(self):
        directory = self.reports()
        report = self.summarize()
        report['elapsed_ms'] = 42
        (directory / 'another.json').write_bytes(json_bytes(report))
        self.assertEqual(len(aggregate_reports(directory)['participants']), 5)
        self.assertEqual(aggregate_reports(directory)['reports'], 6)

    def test_duplicate_report_with_different_whitespace_is_rejected(self):
        directory = self.reports()
        report = json.loads((directory / '0.json').read_bytes())
        (directory / 'copied.json').write_text(json.dumps(report))
        with self.assertRaisesRegex(ToolError, 'Copied'):
            aggregate_reports(directory)

    def test_mixed_candidate_revisions_are_rejected(self):
        directory = self.reports()
        report = self.summarize()
        report['revision'] = 'f' * 40
        (directory / 'other.json').write_bytes(json_bytes(report))
        with self.assertRaisesRegex(ToolError, 'same candidate'):
            aggregate_reports(directory)

    def test_empty_campaign_is_not_reported_as_success(self):
        directory = self.root / 'empty'
        directory.mkdir()
        with self.assertRaises(ToolError):
            aggregate_reports(directory)


if __name__ == '__main__':
    unittest.main()
