import json
import os
from pathlib import Path
import sys
import tempfile
import unittest

from scripts.common import (
    ToolError, bounded_integer, error_json, finite_number, isolated_environment,
    json_bytes, parse_json, read_json, run_bounded, sha256_file, write_new,
)


class CommonTests(unittest.TestCase):
    def test_duplicate_json_keys_fail_closed(self):
        with self.assertRaisesRegex(ToolError, 'duplicate'):
            parse_json('{"passed": false, "passed": true}')

    def test_nonfinite_json_is_rejected(self):
        for value in ('NaN', 'Infinity', '-Infinity'):
            with self.subTest(value=value), self.assertRaises(ToolError):
                parse_json(value)

    def test_exponential_overflow_is_rejected(self):
        with self.assertRaises(ToolError):
            parse_json('{"elapsed_ms": 1e999}')

    def test_boolean_is_not_an_integer(self):
        with self.assertRaises(ToolError):
            bounded_integer(True, 0, 10, 'count')

    def test_invalid_numeric_limits_are_rejected(self):
        for number in (float('nan'), float('inf'), -1, True, '1'):
            with self.subTest(number=number), self.assertRaises(ToolError):
                finite_number(number, 0, 'timeout')

    def test_unrepresentable_integer_limits_are_typed_errors(self):
        with self.assertRaises(ToolError):
            finite_number(10 ** 1000, 0, 'timeout')

    def test_json_is_canonical(self):
        self.assertEqual(json_bytes({'b': 2, 'a': 1}), json_bytes({'a': 1, 'b': 2}))

    def test_exclusive_output_does_not_replace_user_files(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'result.json'
            write_new(path, b'original')
            with self.assertRaises(FileExistsError):
                write_new(path, b'replacement')
            self.assertEqual(path.read_bytes(), b'original')
            if os.name == 'posix':
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_json_input_is_bounded(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'result.json'
            path.write_text('[1, 2, 3]')
            with self.assertRaises(ToolError):
                read_json(path, 4)

    def test_digest_is_content_addressed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'data'
            path.write_bytes(b'abc')
            self.assertEqual(sha256_file(path), 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad')

    def test_process_captures_output_and_exit_status(self):
        with tempfile.TemporaryDirectory() as directory:
            result = run_bounded([sys.executable, '-c', 'import sys; print("out"); print("err",file=sys.stderr); sys.exit(7)'], cwd=Path(directory))
        self.assertEqual(result.returncode, 7)
        self.assertEqual(result.stdout, b'out\n')
        self.assertEqual(result.stderr, b'err\n')
        self.assertIsNone(result.termination)

    def test_process_timeout_is_enforced(self):
        with tempfile.TemporaryDirectory() as directory:
            result = run_bounded([sys.executable, '-c', 'import time; time.sleep(5)'], cwd=Path(directory), timeout=0.1)
        self.assertEqual(result.termination, 'timeout')
        self.assertLess(result.elapsed_ms, 3000)

    def test_process_output_is_bounded_even_after_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            result = run_bounded([sys.executable, '-c', 'import sys; sys.stdout.write("x"*50000)'], cwd=Path(directory), output_limit=64)
        self.assertEqual(result.termination, 'output_limit')
        self.assertLessEqual(len(result.stdout) + len(result.stderr), 64)

    def test_child_environment_excludes_tokens_and_rust(self):
        original = os.environ.get('DOCSIGHT_TEST_SECRET')
        os.environ['DOCSIGHT_TEST_SECRET'] = 'sensitive'
        try:
            environment = isolated_environment()
        finally:
            if original is None:
                del os.environ['DOCSIGHT_TEST_SECRET']
            else:
                os.environ['DOCSIGHT_TEST_SECRET'] = original
        self.assertNotIn('DOCSIGHT_TEST_SECRET', environment)
        self.assertNotIn('cargo', environment['PATH'])

    def test_io_errors_do_not_disclose_paths(self):
        result = json.loads(error_json(FileNotFoundError('/private/customer.docx')))
        self.assertEqual(result['code'], 'IO_ERROR')
        self.assertNotIn('private', json.dumps(result))


if __name__ == '__main__':
    unittest.main()
