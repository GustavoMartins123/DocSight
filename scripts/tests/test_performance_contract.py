import json
from pathlib import Path
import re
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]


class PerformanceContractTests(unittest.TestCase):
    def setUp(self):
        self.budgets = json.loads((ROOT / 'benchmarks/ds9-budgets.json').read_text())
        self.workflow = (ROOT / '.github/workflows/performance.yml').read_text()

    def test_scenarios_match_the_existing_harness(self):
        source = (ROOT / 'xtask/src/main.rs').read_text()
        scenarios = source.split('const SCENARIOS:')[1].split('\n];')[0]
        names = re.findall(r'name: "([^"]+)"', scenarios)
        actual = [item['name'] for item in self.budgets['scenarios']]
        self.assertEqual(sorted(names), sorted(actual))
        self.assertEqual(len(actual), len(set(actual)))

    def test_budgets_and_toolchain_agree(self):
        toolchain = tomllib.loads((ROOT / 'rust-toolchain.toml').read_text())
        self.assertEqual(self.budgets['reference']['rust'], toolchain['toolchain']['channel'])
        self.assertGreater(self.budgets['iterations'], 0)
        for item in self.budgets['scenarios']:
            for key, value in item.items():
                if key != 'name':
                    self.assertGreater(value, 0, key)

    def test_growth_limits_reference_existing_workloads(self):
        names = {item['name'] for item in self.budgets['scenarios']}
        for item in self.budgets['growth_limits']:
            self.assertIn(item['baseline'], names)
            self.assertIn(item['scaled'], names)
            self.assertGreater(item['maximum_peak_memory_ratio'], 0)

    def test_ci_pins_the_host_and_lockfile(self):
        self.assertIn('runs-on: ubuntu-24.04', self.workflow)
        self.assertIn('cargo run --locked --release -p xtask -- benchmark --check', self.workflow)
        self.assertIn('timeout-minutes: 20', self.workflow)
        self.assertIn('cancel-in-progress: true', self.workflow)

    def test_ci_preserves_the_report_on_failure(self):
        self.assertIn('if: always()', self.workflow)
        self.assertIn('path: target/ds9-performance.json', self.workflow)
        self.assertIn('if-no-files-found: error', self.workflow)


if __name__ == '__main__':
    unittest.main()
