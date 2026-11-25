"""Explicit local-only acceptance runner. No live URL, tenant or email target is accepted."""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess
import sys
import time

SVC = Path(__file__).resolve().parent.parent
ROOT = SVC.parent
DATABASE = ROOT / "hrms-database"
UI = ROOT / "hrms-ui"
DOCS = ROOT / "hrms-documentation"
NATIVE_CASES = (
    "email-native", "email-authority-native", "email-recipient-native", "email-reviewers-native",
    "email-organization-reviewers-native", "email-settings-native", "email-key-rotation-native",
    "email-foreign-command-native",
    "email-leave-writers-native", "email-expense-writers-native", "email-tax-writers-native",
    "email-profile-writers-native", "email-timesheet-writers-native", "email-loan-writers-native",
    "email-payroll-writers-native", "email-attendance-writers-native", "email-performance-writers-native",
    "email-delivery-store-native", "email-delivery-native", "email-worker-crash-native",
    "email-worker-metadata-native",
)
NOTIFICATION_CASES = (
    "email_key_rotation", "email_rotation_options", "email_rotation_tls", "email_delivery", "email_supervision", "email_worker_routes", "email_smtp",
    "email_smtp_phases", "email_smtp_wire", "email_templates", "email_settings", "email_test_queue",
)


def commands(suite):
    cases = []
    if suite in ("all", "databases"):
        for name in NATIVE_CASES:
            cases.append((name, DATABASE, ["node", "--experimental-test-isolation=none", "--test",
                          f"provisioning/runtime-grants/tests/{name}.test.cjs"]))
    if suite in ("all", "services"):
        args = ["cargo", "test", "--offline", "--locked", "-p", "kabipay-notification"]
        for name in NOTIFICATION_CASES:
            args.extend(["--test", name])
        cases.append(("notification-contracts", SVC, args))
        cases.append(("employee-email-rbac", SVC, ["cargo", "test", "--offline", "--locked", "-p",
                     "kabipay-employee", "--lib", "services::rbac_access_policy::tests"]))
        cases.append(("organization-email-policy", SVC, ["cargo", "test", "--offline", "--locked", "-p",
                     "kabipay-organization", "--test", "email_policy"]))
        cases.append(("private-email-contracts", SVC, ["cargo", "test", "--offline", "--locked", "-p",
                     "kabipay-organization-tenant", "--test", "email_authority", "--test", "email_identity",
                     "--test", "email_reviewers", "--test", "email_routes", "--test", "email_workflow"]))
    if suite in ("all", "ui"):
        cases.append(("ui-contracts", UI, ["node", "node_modules/vitest/vitest.mjs", "run",
                     "src/modules/organization-workspace", "src/navigation", "src/routes",
                     "src/modules/admin/email", "src/auth/permissionService.test.ts", "--maxWorkers=1",
                     "--testTimeout=30000"]))
        cases.append(("ui-types", UI, ["node", "node_modules/typescript/bin/tsc", "-p", "tsconfig.build.json", "--noEmit"]))
        cases.append(("ui-callers", UI, ["node", "scripts/check-graphql-callers.mjs"]))
    if suite in ("all", "renderer"):
        cases.append(("email-renderer", DOCS, ["node", "--experimental-test-isolation=none", "--test",
                     "scripts/deploy/render.test.mjs"]))
    return cases


def sanitized_env():
    return {key: value for key, value in os.environ.items()
            if not key.upper().startswith(("PG", "POSTGRES", "DATABASE", "KABIPAY"))}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="Execute only the fixed local acceptance commands")
    parser.add_argument("--list", action="store_true", help="Preview commands without executing (the default)")
    parser.add_argument("--suite", choices=("all", "databases", "services", "ui", "renderer"), default="all")
    args = parser.parse_args(argv)
    if args.run and args.list:
        parser.error("Choose preview or execution, not both")
    selected = commands(args.suite)
    if not args.run:
        print(json.dumps({"mode": "preview", "commands": [{"name": name, "cwd": str(cwd), "argv": argv}
              for name, cwd, argv in selected], "externalEmails": False, "productionConnections": False}))
        return 0
    env = sanitized_env()
    env["RUNTIME_GRANTS_PYTHON"] = sys.executable
    env.setdefault("CARGO_TARGET_DIR", str(SVC / "target"))
    if args.suite in ("all", "databases"):
        default_pg = "C:/Program Files/PostgreSQL/17/bin" if os.name == "nt" else ""
        pg = Path(env.get("PRIVATE_TEST_PG_BIN", default_pg))
        if not pg.is_absolute() or not (pg / ("pg_ctl.exe" if os.name == "nt" else "pg_ctl")).is_file():
            parser.error("Set PRIVATE_TEST_PG_BIN to the installed local PostgreSQL binary directory")
        env["PRIVATE_TEST_PG_BIN"] = str(pg)
    directory = ROOT / ".codex-tmp" / ("tenant-email-rehearsal-" + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ"))
    directory.mkdir(parents=True, exist_ok=False)
    results = []
    for name, cwd, argv in selected:
        started = time.monotonic()
        log = directory / f"{name}.log"
        print(json.dumps({"stage": "local_acceptance", "case": name, "log": str(log)}), flush=True)
        with log.open("w", encoding="utf-8") as output:
            try:
                result = subprocess.run(argv, cwd=cwd, env=env, stdout=output, stderr=subprocess.STDOUT,
                                        timeout=2700, check=False)
                code = result.returncode
            except (OSError, subprocess.TimeoutExpired):
                code = 124
        results.append({"case": name, "exitCode": code, "seconds": round(time.monotonic() - started, 2), "log": str(log)})
        report = {"completed": code == 0 and len(results) == len(selected), "results": results,
                  "externalEmails": False, "productionConnections": False, "browserAcceptance": "not_run"}
        (directory / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
        if code:
            print(json.dumps({"completed": False, "case": name, "exitCode": code, "report": str(directory / "report.json")}), flush=True)
            return 1
    print(json.dumps({"completed": True, "cases": len(results), "report": str(directory / "report.json"),
                      "externalEmails": False, "productionConnections": False}), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
