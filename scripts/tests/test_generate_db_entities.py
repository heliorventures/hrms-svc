from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
GENERATOR_PATH = REPOSITORY_ROOT / "hrms-svc/scripts/generate_db_entities.py"
FIXTURE_MIGRATIONS = Path(__file__).parent / "fixtures/survey_privacy"
FIXTURE_MASTER = FIXTURE_MIGRATIONS / "tenant.changelog-master.xml"

SPEC = importlib.util.spec_from_file_location("generate_db_entities", GENERATOR_PATH)
assert SPEC is not None and SPEC.loader is not None
generator = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(generator)


class ForwardAmendmentTests(unittest.TestCase):
    def test_forward_amendments_are_applied_without_consuming_rollback(self) -> None:
        with tempfile.TemporaryDirectory(dir=REPOSITORY_ROOT / ".codex-tmp") as temporary_directory:
            output = Path(temporary_directory)
            with patch.object(generator, "MIGRATIONS", FIXTURE_MIGRATIONS), patch.object(
                generator, "OUT", output
            ), patch.object(
                generator,
                "TENANT_CHANGELOG_MASTER",
                FIXTURE_MASTER,
                create=True,
            ):
                module_name = generator.generate_domain(FIXTURE_MIGRATIONS / "0076_fixture")

            self.assertEqual(module_name, "d0076_fixture")
            generated = (output / "d0076_fixture.rs").read_text(encoding="utf-8")
            self.assertIn("pub completed: bool,", generated)
            self.assertIn("pub publication_department_id: Option<Uuid>,", generated)
            self.assertIn("pub publication_manager_employee_id: Option<Uuid>,", generated)
            self.assertNotIn("completed_at", generated)
            self.assertNotIn("submitted_at", generated)
            self.assertNotIn("master_order_marker", generated)
            self.assertNotIn("unreferenced_draft_column", generated)
            self.assertIn("pub mod unrelated_record", generated)
            self.assertIn("pub label: String,", generated)
            self.assertLess(
                generated.index("0090_order_marker/add.xml"),
                generated.index("0082_fixture/amend.xml"),
            )
            unrelated = generated.split("pub mod unrelated_record", 1)[1]
            self.assertEqual(
                unrelated.count("#[sea_orm(primary_key, auto_increment = false)]"),
                2,
            )

    def test_only_generation_preserves_unrelated_outputs(self) -> None:
        with tempfile.TemporaryDirectory(dir=REPOSITORY_ROOT / ".codex-tmp") as temporary_directory:
            output = Path(temporary_directory)
            unrelated_path = output / "d9999_existing.rs"
            unrelated_path.write_text("user-owned generated output\n", encoding="utf-8")
            mod_path = output / "mod.rs"
            mod_path.write_text("pub mod d9999_existing;\n", encoding="utf-8")

            with patch.object(generator, "MIGRATIONS", FIXTURE_MIGRATIONS), patch.object(
                generator, "OUT", output
            ), patch.object(
                generator,
                "TENANT_CHANGELOG_MASTER",
                FIXTURE_MASTER,
                create=True,
            ), patch.object(sys, "argv", [str(GENERATOR_PATH), "--only", "0076_fixture"]):
                generator.main()

            self.assertEqual(
                unrelated_path.read_text(encoding="utf-8"),
                "user-owned generated output\n",
            )
            self.assertFalse((output / "prelude.rs").exists())
            exports = mod_path.read_text(encoding="utf-8")
            self.assertIn("pub mod d0076_fixture;", exports)
            self.assertIn("pub mod d9999_existing;", exports)


if __name__ == "__main__":
    unittest.main()
