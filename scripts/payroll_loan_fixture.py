"""Isolated upstream adapters plus actual payslip snapshot migration for Phase B tests."""
from pathlib import Path
import importlib.util
import re
import sys
root = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('loan_sql', root.parent / 'hrms-database/tests/migrations/loan_fixture_sql.py')
loan_sql = importlib.util.module_from_spec(spec)
spec.loader.exec_module(loan_sql)
types = {'Uuid':'UUID','String':'TEXT','bool':'BOOLEAN','i32':'INT','i64':'BIGINT','Decimal':'NUMERIC','DateTimeUtc':'TIMESTAMPTZ','NaiveDate':'DATE','Json':'JSONB','i16':'SMALLINT'}
print('BEGIN;')
print('ALTER TABLE loan_test.employee ADD COLUMN imported_exit_date DATE, ADD COLUMN imported_last_working_date DATE, ADD COLUMN payroll_excluded BOOLEAN NOT NULL DEFAULT FALSE;')
# Real model fields, disposable defaults only. Constraints specific to payroll evidence
# come from the actual Liquibase migrations appended below.
created = {'employee','payslip_loan_snapshot'}
for domain, names in {
    'd0005_auth_rbac': ['user'],
    'd0012_payroll': ['payroll_cycle','payslip','payslip_component','salary_component','salary_structure','salary_structure_component','employee_salary_structure','employee_salary_component_override'],
    'd0090_payroll_period_configuration': ['payroll_period_input','payslip_statement','employee_payroll_rule'],
    'd0093_tax_projection_configuration': ['company_payroll_rule','employee_tax_settings','employee_tax_declaration','employee_tax_history'],
    'd0097_location_working_calendar': ['working_calendar_profile','employee_location_assignment','weekly_off_policy_version'],
    'd0098_leave_working_dates': ['leave_working_date_snapshot'],
    'd0077_unpaid_leave_payroll': ['payroll_unpaid_leave_allocation','payroll_unpaid_leave_policy'],
    'd0010_time_shift_roster': ['holiday','holiday_calendar'],
    'd0031_tax_proof': ['tax_proof_line'],
    'd0013_tax_statutory': ['tax_configuration_version','tax_slab','tax_section_definition'],
    'd0011_leave': ['leave_type','leave_request'],
    'd0035_payroll_arrear': ['payroll_arrear'],
    'd0017_onboarding_offboarding': ['separation','fnf_settlement','clearance_checklist'],
}.items():
    text = (root / f'crates/kabipay-db-entities/src/tenant/{domain}.rs').read_text(encoding='utf-8')
    for name in names:
        block = text.split(f'pub mod {name} {{',1)[1]
        body = re.search(r'pub struct Model\s*\{(.*?)\n\s*\}',block,re.S).group(1)
        columns = []
        for field, kind in re.findall(r'pub (?:r#)?(\w+):\s*([^,]+),',body):
            optional=kind.startswith('Option<')
            kind=kind[7:-1] if optional else kind
            default={'Uuid':'gen_random_uuid()','String':"''",'bool':'FALSE','i32':'0','i64':'0','Decimal':'0','DateTimeUtc':'NOW()','NaiveDate':'CURRENT_DATE','Json':"'{}'::jsonb",'i16':'0'}[kind]
            columns.append(f'"{field}" {types[kind]}' + ('' if optional else f' NOT NULL DEFAULT {default}'))
        keys=re.findall(r'#\[sea_orm\([^\]]*\bprimary_key\b[^\]]*\)\]\s*pub (?:r#)?(\w+):',body)
        if not keys: raise ValueError('missing fixture primary key: '+name)
        columns.append('PRIMARY KEY('+','.join('"'+key+'"' for key in keys)+')')
        created.add(name)
        print(f'CREATE TABLE loan_test.{name} (' + ','.join(columns) + ');')
# Fingerprint's existing input set requires these upstream names, not their contents.
# The input contract now belongs to the fixed database authority; runtime
# Payroll intentionally has no reflected table list or broad table reads.
sys.path.insert(0, str(root.parent / 'hrms-database/provisioning/runtime-grants'))
from payroll_authority import INPUT_TABLES
tables = INPUT_TABLES
missing=set(tables)-created
if missing: raise ValueError('missing authoritative input fixture models: '+','.join(sorted(missing)))
print('CREATE TABLE loan_test.payroll_draft_calculation(tenant_id UUID,cycle_id UUID,revision INT,fingerprint TEXT,snapshot JSONB,calculated_by UUID,calculated_by_principal_id UUID,calculated_at TIMESTAMPTZ DEFAULT NOW(),finalized_at TIMESTAMPTZ,finalized_by UUID,finalized_by_principal_id UUID,acknowledgement JSONB,PRIMARY KEY(tenant_id,cycle_id));')
print(loan_sql.migration_sql('changelog/migrations/0105_payslip_loan_evidence/payslip_loan_evidence.xml'))
print(loan_sql.migration_sql('changelog/migrations/0106_loan_exit_coordination/loan_exit_coordination.xml'))
print(loan_sql.migration_sql('changelog/migrations/0107_loan_reconciliation/loan_reconciliation.xml'))
print(loan_sql.migration_sql('changelog/migrations/0108_loan_late_payments/loan_late_payments.xml'))
print(loan_sql.migration_sql('changelog/migrations/0109_loan_schedule_anchor/loan_schedule_anchor.xml'))
print('ALTER TABLE loan_test.payroll_cycle ADD COLUMN processed_by_principal_id UUID;')
print('COMMIT;')
