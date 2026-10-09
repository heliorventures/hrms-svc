"""Isolated upstream adapters plus actual payslip snapshot migration for Phase B tests."""
from pathlib import Path
import importlib.util
import re
root = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('loan_sql', root.parent / 'hrms-database/tests/migrations/loan_fixture_sql.py')
loan_sql = importlib.util.module_from_spec(spec)
spec.loader.exec_module(loan_sql)
types = {'Uuid':'UUID','String':'TEXT','bool':'BOOLEAN','i32':'INT','i64':'BIGINT','Decimal':'NUMERIC','DateTimeUtc':'TIMESTAMPTZ','NaiveDate':'DATE','Json':'JSONB'}
print('BEGIN;')
print('ALTER TABLE loan_test.employee ADD COLUMN imported_exit_date DATE, ADD COLUMN imported_last_working_date DATE, ADD COLUMN payroll_excluded BOOLEAN NOT NULL DEFAULT FALSE;')
# Real model fields, disposable defaults only. Constraints specific to payroll evidence
# come from the actual Liquibase migrations appended below.
for domain, names in {
    'd0012_payroll': ['payroll_cycle','payslip','payslip_component','salary_component'],
    'd0090_payroll_period_configuration': ['payroll_period_input','payslip_statement'],
    'd0011_leave': ['leave_type'],
    'd0035_payroll_arrear': ['payroll_arrear'],
}.items():
    text = (root / f'crates/kabipay-db-entities/src/tenant/{domain}.rs').read_text(encoding='utf-8')
    for name in names:
        block = text.split(f'pub mod {name} {{',1)[1]
        body = re.search(r'pub struct Model\s*\{(.*?)\n\s*\}',block,re.S).group(1)
        columns = []
        for field, kind in re.findall(r'pub (?:r#)?(\w+):\s*([^,]+),',body):
            optional=kind.startswith('Option<')
            kind=kind[7:-1] if optional else kind
            default={'Uuid':'gen_random_uuid()','String':"''",'bool':'FALSE','i32':'0','i64':'0','Decimal':'0','DateTimeUtc':'NOW()','NaiveDate':'CURRENT_DATE','Json':"'{}'::jsonb"}[kind]
            columns.append(f'"{field}" {types[kind]}' + ('' if optional else f' NOT NULL DEFAULT {default}'))
        key = 'payslip_id' if name == 'payslip_statement' else 'id'
        columns.append(f'PRIMARY KEY({key})')
        print(f'CREATE TABLE loan_test.{name} (' + ','.join(columns) + ');')
# Fingerprint's existing input set requires these upstream names, not their contents.
fingerprint=(root / 'crates/kabipay-payroll/src/services/payroll_fingerprint.rs').read_text(encoding='utf-8')
tables=re.findall(r'"([a-z_]+)"',fingerprint.split('const INPUT_TABLES')[1].split('];')[0])
for table in tables:
    if table not in ('employee','payroll_cycle','payslip','payslip_component','payslip_statement','payslip_loan_snapshot','payroll_period_input','salary_component','leave_type','payroll_arrear'):
        extra=',calendar_id UUID' if table=='holiday' else ''
        print(f'CREATE TABLE loan_test.{table}(id UUID DEFAULT gen_random_uuid(),tenant_id UUID{extra});')
print('CREATE TABLE loan_test.payroll_draft_calculation(tenant_id UUID,cycle_id UUID,revision INT,fingerprint TEXT,snapshot JSONB,calculated_by UUID,calculated_at TIMESTAMPTZ DEFAULT NOW(),finalized_at TIMESTAMPTZ,finalized_by UUID,acknowledgement JSONB,PRIMARY KEY(tenant_id,cycle_id));')
print(loan_sql.migration_sql('changelog/migrations/0105_payslip_loan_evidence/payslip_loan_evidence.xml'))
print('COMMIT;')
