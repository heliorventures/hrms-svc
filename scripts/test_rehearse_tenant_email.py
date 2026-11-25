import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import unittest
from unittest.mock import patch


def subject():
    path = Path(__file__).with_name("rehearse-tenant-email.py")
    spec = importlib.util.spec_from_file_location("email_rehearsal", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class RehearsalAdmissionTests(unittest.TestCase):
    def test_preview_launches_nothing_and_exposes_only_fixed_local_commands(self):
        module = subject()
        with patch.object(module.subprocess, "run", side_effect=AssertionError("must not execute")):
            with contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(module.main([]), 0)
        report = json.loads(output.getvalue())
        self.assertEqual(report["mode"], "preview")
        self.assertGreater(len(report["commands"]), 10)
        self.assertFalse(report["externalEmails"])
        self.assertFalse(report["productionConnections"])
        self.assertNotIn("DATABASE_URL", output.getvalue())

    def test_database_provider_and_live_target_arguments_are_not_accepted(self):
        module = subject()
        with contextlib.redirect_stderr(io.StringIO()):
            for flags in [["--database-url", "postgres://secret"], ["--tenant", "production"],
                          ["--send-email"], ["--run", "--list"]]:
                with self.assertRaises(SystemExit):
                    module.main(flags)

    def test_external_database_and_service_configuration_are_not_inherited(self):
        module = subject()
        with patch.dict(os.environ, {"DATABASE_URL": "secret", "PGHOST": "production",
                                     "KABIPAY_EMAIL_KEYRING_FILE": "private", "POSTGRES_PASSWORD": "private"}):
            env = module.sanitized_env()
        self.assertFalse(any(key.upper().startswith(("PG", "POSTGRES", "DATABASE", "KABIPAY")) for key in env))


if __name__ == "__main__":
    unittest.main()
