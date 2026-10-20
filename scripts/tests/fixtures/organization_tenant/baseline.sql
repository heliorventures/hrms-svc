-- Synthetic prerequisite tables for exercising 0112/0115 PostgreSQL SQL and
-- canonical facts. This is not a substitute for a full Liquibase tenant upgrade.
CREATE SCHEMA source_acceptance;
SET search_path=source_acceptance,public;
CREATE TABLE employee(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,user_id UUID,first_name TEXT NOT NULL,last_name TEXT NOT NULL,employee_code TEXT NOT NULL,status TEXT NOT NULL,is_deleted BOOLEAN NOT NULL DEFAULT false,department_id UUID,designation_id UUID,reporting_manager_id UUID);
CREATE TABLE leave_request(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,employee_id UUID NOT NULL,status TEXT NOT NULL,is_deleted BOOLEAN NOT NULL DEFAULT false,reason TEXT);
CREATE TABLE timesheet_week_batch(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,employee_id UUID NOT NULL,status TEXT NOT NULL,workflow_instance_id UUID);
CREATE TABLE timesheet_entry(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,employee_id UUID NOT NULL,work_date DATE NOT NULL,hours_worked NUMERIC NOT NULL,status TEXT NOT NULL,is_deleted BOOLEAN NOT NULL DEFAULT false);
CREATE TABLE performance_participant(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,employee_id UUID NOT NULL,status TEXT NOT NULL,is_excluded BOOLEAN NOT NULL DEFAULT false,manager_submitted_at TIMESTAMPTZ,manager_employee_id UUID,review_cycle_id UUID);
CREATE TABLE continuous_feedback(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,reviewee_employee_id UUID NOT NULL,created_by_user_id UUID NOT NULL,comments TEXT);
CREATE TABLE workflow_instance(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,entity_id UUID NOT NULL,entity_type TEXT NOT NULL,status TEXT NOT NULL,current_step_id UUID);
CREATE TABLE payroll_cycle(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,status TEXT NOT NULL,processed_by UUID);
CREATE TABLE payroll_draft_calculation(tenant_id UUID NOT NULL,cycle_id UUID NOT NULL,calculated_by UUID NOT NULL,finalized_by UUID);
CREATE TABLE loan_posting(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,actor_id UUID NOT NULL);
CREATE TABLE loan_audit_event(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,actor_id UUID NOT NULL);
CREATE TABLE payroll_period_input(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,updated_by UUID NOT NULL);
CREATE TABLE payroll_period_input_audit(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,changed_by UUID NOT NULL);
CREATE TABLE payroll_period_adjustment(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,updated_by UUID NOT NULL);

CREATE TABLE workflow_action(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,instance_id UUID NOT NULL,performed_by UUID,acted_at TIMESTAMPTZ NOT NULL);

CREATE TABLE review_cycle(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,current_stage TEXT NOT NULL);
