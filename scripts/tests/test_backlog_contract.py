import unittest

from scripts.common import ROOT, read_json


class BacklogContractTests(unittest.TestCase):
    def test_incremental_pdf_entry_points_to_existing_code_and_regression(self):
        backlog = (ROOT / 'BACKLOG.md').read_text()
        parser = (ROOT / 'crates/docsight-pdf/src/syntax.rs').read_text()
        tests = (ROOT / 'crates/docsight-pdf/tests/pdf.rs').read_text()
        self.assertNotIn('| Incremental PDF updates', backlog)
        self.assertIn('reads_cross_reference_streams_with_compressed_objects', backlog)
        self.assertIn('fn reads_cross_reference_streams_with_compressed_objects', tests)
        self.assertIn('incremental_update: true', tests)
        self.assertIn('"Prev"', parser)

    def test_actual_engine_gaps_are_not_closed_by_distribution_tooling(self):
        gaps = read_json(ROOT / 'release/known-gaps.json')
        blocking = {item['id']: item['status'] for item in gaps['items'] if item['blocking']}
        self.assertEqual(blocking, {'docx-multi-section-geometry': 'open',
                                    'docx-line-level-pagination': 'open',
                                    'content-addressed-cache': 'open'})
        self.assertIn('DOCX_SECTIONS_COLLAPSED', (ROOT / 'crates/docsight-ooxml/src/parser.rs').read_text())
        self.assertIn('DOCX_PAGINATION_BLOCK_GRANULAR', (ROOT / 'crates/docsight-layout/src/layout.rs').read_text())

    def test_distribution_docs_do_not_declare_unexecuted_native_tests_passed(self):
        install = (ROOT / 'INSTALL.md').read_text()
        self.assertIn('required native-test baselines', install)
        self.assertNotIn('These are the tested operating-system baselines', install)
        self.assertIn('DOCX PNG and JPEG', (ROOT / 'BACKLOG.md').read_text())


if __name__ == '__main__':
    unittest.main()
