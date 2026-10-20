export const ADMINISTRATION_ACTIONS=Object.freeze({
 employee:['employee_create','employee_update','employee_employment','employee_login_provision'],
 user:['account_create','account_update','account_access','account_roles'],
 workflow_definition:['workflow_save','workflow_activation'],
 separation:['separation_create','separation_reject','separation_cancel_approval'],
 clearance_checklist:['clearance_update'],
 payroll_job:['payroll_job_status','payroll_job_cancel_before_execution']
});
export const COMMAND_OPERATIONS=Object.freeze([
 ...Object.values(ADMINISTRATION_ACTIONS).flat().filter(action=>action!=='payroll_job_status'),
 'employee_detail','employee_list','employee_references','account_list','workflow_list','workflow_detail',
 'separation_list','separation_detail','payroll_job_list','payroll_job_detail',
 'install_membership','install_grant','directory','install_relationship','leave_detail','leave_decision',
 'feedback_create','review_detail','review_submit','timesheet_detail','timesheet_decision',
 'payroll_list','payroll_detail','payroll_draft','payroll_finalize','read_receipt','cancel_command'
]);
