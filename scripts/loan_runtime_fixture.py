"""Disposable test adapters for pre-existing Employee, Workflow and Ops entities.

Loans tables/constraints are taken from the actual Liquibase migration. These adapters
only supply upstream tables needed by module integration tests, not deployment SQL.
"""
from pathlib import Path
import re
ROOT=Path(__file__).resolve().parents[1]
TYPES={'Uuid':('UUID','gen_random_uuid()'),'String':('TEXT',"''"),'bool':('BOOLEAN','FALSE'),'i32':('INT','0'),'i64':('BIGINT','0'),'i16':('SMALLINT','0'),'Decimal':('NUMERIC','0'),'DateTimeUtc':('TIMESTAMPTZ','NOW()'),'NaiveDate':('DATE','CURRENT_DATE'),'Json':('JSONB',"'{}'::jsonb")}
def fields(source):
    body=re.search(r'pub struct Model\s*\{(.*?)\n\s*\}',source,re.S).group(1)
    for name,kind in re.findall(r'pub (\w+):\s*([^,]+),',body):
        optional=kind.startswith('Option<');kind=kind[7:-1] if optional else kind
        sql,default=TYPES[kind]
        yield name,sql+('' if optional else ' NOT NULL DEFAULT '+default)
print('\\set ON_ERROR_STOP on\nBEGIN;\nCREATE SCHEMA kabipay_ops;')
for name in ('tenant','module','tenant_subscription','feature_flag'):
    source=(ROOT/f'crates/kabipay-db-entities/src/ops/{name}.rs').read_text(encoding='utf-8')
    print(f'CREATE TABLE kabipay_ops.{name} ('+', '.join(f'"{key}" {declaration}' for key,declaration in fields(source))+');')
print('CREATE TABLE kabipay_ops.module_dependency(module_id UUID NOT NULL,depends_on_module_id UUID NOT NULL);')
source=(ROOT/'crates/kabipay-db-entities/src/tenant/d0007_employee_core.rs').read_text(encoding='utf-8')
for name,declaration in fields(source):
    if name not in ('id','tenant_id'): print(f'ALTER TABLE loan_test.employee ADD COLUMN "{name}" {declaration};')
source=(ROOT/'crates/kabipay-db-entities/src/tenant/d0025_workflow.rs').read_text(encoding='utf-8')
for name in ('workflow','workflow_step','workflow_instance','workflow_action'):
    section=source.split(f'pub mod {name} {{',1)[1]
    print(f'CREATE TABLE loan_test.{name} ('+', '.join(f'"{key}" {declaration}' for key,declaration in fields(section))+');')
print('COMMIT;')
