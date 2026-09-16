import unittest

from scripts.beta import REPORT_FIELDS
from scripts.common import ROOT, read_json
from scripts.readiness import CRITERIA
from scripts.release import SMOKE_CHECKS, TARGETS
from scripts.validate import CHECK_NAMES


class ToolingSchemaTests(unittest.TestCase):
    def setUp(self):
        self.schema = read_json(ROOT / 'schemas/tooling/v1/evidence.json')
        self.definitions = self.schema['$defs']

    def test_beta_schema_tracks_the_exact_privacy_projection(self):
        definition = self.definitions['beta-report']
        self.assertEqual(set(definition['required']), REPORT_FIELDS)
        self.assertFalse(definition['additionalProperties'])
        self.assertEqual(set(definition['properties']), REPORT_FIELDS)

    def test_target_matrix_and_required_gate_names_are_synchronized(self):
        self.assertEqual(set(self.definitions['release-manifest']['properties']['target']['enum']), set(TARGETS))
        for name, field, expected in (('smoke-report', 'checks', SMOKE_CHECKS),
                                     ('validation-report', 'checks', CHECK_NAMES),
                                     ('readiness-report', 'criteria', CRITERIA)):
            specification = self.definitions[name]['properties'][field]
            self.assertEqual(specification['minItems'], len(expected))
            self.assertEqual(specification['maxItems'], len(expected))
            self.assertEqual(specification['items']['properties']['name']['enum'], list(expected))

    def test_each_artifact_has_a_unique_versioned_schema_and_closed_fields(self):
        identifiers = []
        for name, definition in self.definitions.items():
            if name in ('version', 'revision'):
                continue
            self.assertFalse(definition['additionalProperties'])
            self.assertEqual(set(definition['required']), set(definition['properties']))
            identifier = definition['properties']['schema']['const']
            self.assertTrue(identifier.endswith('/v1'))
            identifiers.append(identifier)
        self.assertEqual(len(identifiers), len(set(identifiers)))
        self.assertEqual(len(self.schema['oneOf']), len(identifiers))


if __name__ == '__main__':
    unittest.main()
