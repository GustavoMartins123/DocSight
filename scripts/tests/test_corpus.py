import copy
import json
from pathlib import Path
import unittest
from unittest.mock import patch

from scripts.common import ROOT, ProcessResult, ToolError, json_bytes, sha256_file
from scripts.corpus import evaluate, json_pointer, load_manifest, run_corpus
from scripts.tests.test_release import ReleaseFixture
from scripts.tests.test_smoke import sample_png


class CorpusTests(ReleaseFixture):
    def setUp(self):
        super().setUp()
        document = self.root / 'case.docx'
        document.write_bytes(b'synthetic document')
        self.case = {'id': 'sample-inspect', 'file': 'case.docx', 'sha256': sha256_file(document),
                     'origin': 'synthetic', 'format': 'docx', 'operation': 'inspect',
                     'expected': {'exit_code': 0, 'diagnostic_codes': [], 'pointer_equals': {'/result/format': 'docx'}, 'repeat': 2}}
        self.manifest_path = self.root / 'corpus.json'
        self.calls = []
        self.nonce = False
        self.crash = False
        self.write_manifest()

    def write_manifest(self, cases=None):
        self.manifest_path.write_bytes(json_bytes({'schema': 'docsight.corpus/v1', 'cases': cases or [self.case]}))

    def engine(self, arguments, **options):
        self.calls.append(arguments)
        if '--version' in arguments:
            return ProcessResult(0, b'docsight 0.1.4\n', b'', 1)
        if self.crash and 'inspect' in arguments:
            return ProcessResult(-11, b'', b'private crash stack', 2)
        result = {'format': 'docx', 'tables': 0}
        if self.nonce:
            result['nonce'] = len(self.calls)
        if 'render' in arguments:
            Path(arguments[arguments.index('--out') + 1]).write_bytes(sample_png())
        return ProcessResult(0, json_bytes({'schema': 'docsight.agent/v2', 'result': result}), b'', 4)

    def run_campaign(self):
        with patch('scripts.corpus.native_target', return_value='x86_64-unknown-linux-gnu'):
            return run_corpus(self.package(), self.manifest_path, self.root, self.engine)

    def test_checked_in_manifest_matches_all_existing_fixtures(self):
        manifest = load_manifest(ROOT / 'release/corpus.json', ROOT)
        self.assertEqual(len(manifest['cases']), 12)
        self.assertEqual({case['origin'] for case in manifest['cases']}, {'synthetic'})
        self.assertEqual({case['format'] for case in manifest['cases']}, {'pdf', 'docx', 'invalid'})
        self.assertTrue(any(case['expected']['exit_code'] == 10 for case in manifest['cases']))

    def test_all_cases_are_validated_before_any_execution(self):
        (self.root / 'case.docx').write_bytes(b'changed after review')
        with self.assertRaisesRegex(ToolError, 'digest'):
            self.run_campaign()
        self.assertEqual(self.calls, [])

    def test_path_traversal_is_rejected(self):
        for name in ('../private.pdf', '/tmp/private.pdf', 'C:/private.pdf', 'a\\b.docx'):
            self.case['file'] = name
            self.write_manifest()
            with self.subTest(name=name), self.assertRaises(ToolError):
                load_manifest(self.manifest_path, self.root)

    def test_unknown_keys_and_duplicate_ids_are_rejected(self):
        self.case['password'] = 'never store a password'
        self.write_manifest()
        with self.assertRaises(ToolError):
            load_manifest(self.manifest_path, self.root)
        self.case.pop('password')
        self.write_manifest([self.case, self.case])
        with self.assertRaises(ToolError):
            load_manifest(self.manifest_path, self.root)

    def test_negative_cases_require_typed_diagnostics(self):
        self.case['expected']['exit_code'] = 10
        self.write_manifest()
        with self.assertRaises(ToolError):
            load_manifest(self.manifest_path, self.root)

    def test_success_is_bound_to_manifest_archive_and_repeated_output(self):
        report = self.run_campaign()
        self.assertTrue(report['passed'])
        self.assertEqual(report['cases'][0]['attempts'], 2)
        self.assertEqual(report['manifest_sha256'], sha256_file(self.manifest_path))
        self.assertNotIn('synthetic document', json.dumps(report))
        self.assertTrue(all('--sandbox' in arguments for arguments in self.calls[1:]))

    def test_repeated_output_changes_fail(self):
        self.nonce = True
        report = self.run_campaign()
        self.assertFalse(report['passed'])
        self.assertEqual(report['cases'][0]['error_code'], 'CORPUS_NONDETERMINISTIC')

    def test_crash_is_recorded_and_other_cases_still_run(self):
        self.crash = True
        second = copy.deepcopy(self.case)
        second.update(id='sample-render', operation='render')
        self.write_manifest([self.case, second])
        report = self.run_campaign()
        self.assertFalse(report['passed'])
        self.assertEqual(report['cases'][0]['error_code'], 'CORPUS_CRASH')
        self.assertTrue(report['cases'][1]['passed'])
        self.assertNotIn('private crash stack', json.dumps(report))

    def test_diff_uses_the_explicit_second_document_and_records_its_digest(self):
        reference = self.root / 'revised.docx'
        reference.write_bytes(b'different synthetic document')
        self.case.update(operation='diff', reference={'file': reference.name, 'sha256': sha256_file(reference)})
        self.write_manifest()
        report = self.run_campaign()
        self.assertTrue(report['passed'])
        self.assertEqual(report['cases'][0]['reference_sha256'], sha256_file(reference))
        self.assertEqual(self.calls[-1][-2:], [str(self.root / 'case.docx'), str(reference)])

    def test_second_document_digest_is_verified_before_execution(self):
        reference = self.root / 'revised.docx'
        reference.write_bytes(b'different synthetic document')
        self.case.update(operation='diff', reference={'file': reference.name, 'sha256': '0' * 64})
        self.write_manifest()
        with self.assertRaisesRegex(ToolError, 'digest'):
            self.run_campaign()
        self.assertEqual(self.calls, [])

    def test_input_changes_during_the_final_attempt_fail_closed(self):
        for changed in ('document', 'reference'):
            with self.subTest(changed=changed):
                document = self.root / 'case.docx'
                reference = self.root / 'revised.docx'
                document.write_bytes(b'synthetic document')
                reference.write_bytes(b'synthetic reference')
                self.case.update(operation='diff', sha256=sha256_file(document),
                                 reference={'file': reference.name, 'sha256': sha256_file(reference)})
                self.case['expected']['repeat'] = 1
                self.write_manifest()
                original_engine = self.engine

                def changing_engine(arguments, **options):
                    result = original_engine(arguments, **options)
                    if '--version' not in arguments:
                        (document if changed == 'document' else reference).write_bytes(b'changed during execution')
                    return result

                with patch('scripts.corpus.native_target', return_value='x86_64-unknown-linux-gnu'):
                    report = run_corpus(self.package(out=f"changed-{changed}"), self.manifest_path, self.root, changing_engine)
                self.assertFalse(report['passed'])
                self.assertEqual(report['cases'][0]['error_code'], 'CORPUS_DIGEST_MISMATCH')
                self.assertEqual(report['cases'][0]['attempts'], 1)

    def test_non_diff_cases_reject_unused_reference_inputs(self):
        self.case['reference'] = {'file': 'case.docx', 'sha256': self.case['sha256']}
        self.write_manifest()
        with self.assertRaisesRegex(ToolError, 'second document'):
            load_manifest(self.manifest_path, self.root)

    def test_render_is_checked_and_repeated(self):
        self.case['operation'] = 'render'
        self.write_manifest()
        report = self.run_campaign()
        self.assertTrue(report['passed'])
        self.assertEqual(report['cases'][0]['attempts'], 2)

    def test_expected_assertion_is_not_satisfied_by_wrong_type(self):
        expected = copy.deepcopy(self.case['expected'])
        expected['pointer_equals'] = {'/result/tables': 0}
        result = ProcessResult(0, json_bytes({'schema': 'docsight.agent/v2', 'result': {'tables': False}}), b'', 2)
        with self.assertRaisesRegex(ToolError, 'assertion'):
            evaluate(result, expected)

    def test_missing_json_pointer_is_not_json_null(self):
        with self.assertRaises(ToolError):
            json_pointer({'result': {}}, '/result/absent')
        self.assertIsNone(json_pointer({'result': {'value': None}}, '/result/value'))
        self.assertEqual(json_pointer({'a/b': {'~name': [42]}}, '/a~1b/~0name/0'), 42)

    def test_typed_negative_case_passes_only_with_expected_code(self):
        expected = {'exit_code': 10, 'diagnostic_codes': ['UNSUPPORTED_FORMAT'], 'pointer_equals': {}, 'repeat': 2}
        result = ProcessResult(10, b'', json_bytes({'schema': 'docsight.agent/v2',
                               'error': {'code': 'UNSUPPORTED_FORMAT', 'exit_code': 10}}), 2)
        self.assertEqual(evaluate(result, expected), ['UNSUPPORTED_FORMAT'])
        expected['diagnostic_codes'] = ['PDF_PASSWORD_REQUIRED']
        with self.assertRaises(ToolError):
            evaluate(result, expected)

    def test_clean_stdout_stderr_contract_is_enforced(self):
        for result in (ProcessResult(0, b'{}', b'', 1), ProcessResult(0, self.engine(['inspect']).stdout, b'log', 1),
                       ProcessResult(-9, b'', b'', 1, 'timeout')):
            with self.subTest(result=result), self.assertRaises(ToolError):
                evaluate(result, self.case['expected'])


if __name__ == '__main__':
    unittest.main()
