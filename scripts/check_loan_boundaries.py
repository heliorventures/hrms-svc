"""Enforce private Loans persistence and caller-owned transactions."""
from pathlib import Path
import re
root=Path(__file__).resolve().parents[1]
violations=[]
storage=re.compile(r'd0103_employee_loans|d0107_loan_reconciliation|d0106_loan_exit_coordination::loan_|\b(?:FROM|JOIN|UPDATE|INTO)\s+(?:["\w]+\.)?"?loan_(?:account|request|policy_version|posting|ledger_entry|terms_version|schedule_version|schedule_item|receipt|disbursement|allocation|period_override|employee_state|command_receipt|audit_event|internal_outbox|unapplied_credit|exit_review|exit_approval_guard|exit_arrangement|reconciliation_run|reconciliation_status|consumer_receipt)\b',re.I)
for crate in ('kabipay-payroll','kabipay-employee'):
    for file in (root/'crates'/crate/'src').rglob('*.rs'):
        if storage.search(file.read_text(encoding='utf-8')): violations.append(str(file.relative_to(root)))
for file in (root/'crates/kabipay-loans/src').rglob('*.rs'):
    source=file.read_text(encoding='utf-8')
    if re.search(r'\.(?:commit|begin)\s*\(|Database::connect|reqwest::|hyper::Client',source): violations.append(str(file.relative_to(root)))
for file in (root/'crates/kabipay-employee/src').rglob('*.rs'):
    if re.search(r'\b(?:fnf_settlement|fnf_financial_review|fnf_payout_source)\b', file.read_text(encoding='utf-8')):
        violations.append(str(file.relative_to(root)))
if violations: raise SystemExit('Loans boundary violation: '+', '.join(violations))
print('Loans storage and transaction boundaries passed')
