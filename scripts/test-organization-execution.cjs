// Disposable real HTTP integration fixture. It refuses all non-loopback and non-fixture databases.
// Run with: node scripts/test-organization-execution.cjs
const assert = require('node:assert/strict');
const { spawn, spawnSync } = require('node:child_process');
const { generateKeyPairSync, randomUUID, createSign } = require('node:crypto');
const fs = require('node:fs');
const net = require('node:net');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const { Client } = require('../../hrms-database/node_modules/pg');
const { runPayrollJobAcceptance } = require('./test-organization-payroll-jobs.cjs');
const { runHrAdministrationAcceptance } = require('./test-organization-hr-administration.cjs');
const { prepareOrganizationReportingAcceptance } = require('./test-organization-reporting.cjs');

const repo = path.resolve(__dirname, '..', '..');
const root = path.resolve(__dirname, '..');
const fixtures = path.join(__dirname, 'tests', 'fixtures', 'organization_execution');
const runId = randomUUID().replaceAll('-', '');
const container = `hrms-org-execution-${runId}`;
const temp = path.join(root, '.superpowers', `organization-execution-${runId}`);
const databases = ['org_execution_fixture', 'organization_company_a_fixture', 'organization_company_b_fixture'];
const ids = Object.fromEntries(['a', 'b', 'aUser', 'bUser', 'aEmployee', 'bEmployee', 'leave', 'leaveType', 'workflow', 'step', 'instance', 'payrollCycle', 'selfAdmin', 'adminRole', 'staffRole'].map(k => [k, randomUUID()]));
const clients = [];
const children = [];
let leaveProxy, dropNextLeaveDecision = false;
let created = false;
let pgPort;
let reportingAcceptance;

function docker(args) {
  const r = spawnSync('docker', args, { encoding: 'utf8', windowsHide: true, timeout: 60000 });
  if (r.error || r.status !== 0) throw Error(`disposable Docker command failed: ${r.error?.message || r.stderr?.trim() || r.status}`);
  return r.stdout.trim();
}
function delay(ms) { return new Promise(resolve => setTimeout(resolve, ms)); }
async function port() {
  const server = net.createServer();
  await new Promise((resolve, reject) => server.listen(0, '127.0.0.1', resolve).once('error', reject));
  const value = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return value;
}
function key(label) {
  const { privateKey, publicKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
  const privateFile = path.join(temp, `${label}.private.pem`);
  const publicFile = path.join(temp, `${label}.public.pem`);
  fs.writeFileSync(privateFile, privateKey.export({ type: 'pkcs8', format: 'pem' }), { mode: 0o600 });
  fs.writeFileSync(publicFile, publicKey.export({ type: 'spki', format: 'pem' }), { mode: 0o600 });
  return { kid: `fixture-${label}`, privateFile, publicFile, privateKey };
}
function jwt(company, user, signer, employee = company === ids.a ? ids.aEmployee : ids.bEmployee) {
  const now = Math.floor(Date.now() / 1000);
  const header = { alg: 'RS256', typ: 'hrms-client+jwt', kid: signer.kid };
  const body = { sub: user, tenant_id: company, iss: 'kabipay-client', aud: 'kabipay-client-api', nbf: now, iat: now, exp: now + 3600, email: '', employee_id: employee, must_change_password: false, roles: [], permissions: [], permission_scopes: {}, resource_scopes: {} };
  const part = value => Buffer.from(JSON.stringify(value)).toString('base64url');
  const input = `${part(header)}.${part(body)}`;
  const signature = createSign('RSA-SHA256').update(input).end().sign(signer.privateKey).toString('base64url');
  return `${input}.${signature}`;
}
function dbUrl(name) {
  assert(databases.includes(name));
  return `postgres://organization_fixture:synthetic@127.0.0.1:${pgPort}/${name}?sslmode=disable`;
}
async function db(name) {
  const c = new Client({ connectionString: dbUrl(name) });
  await c.connect(); clients.push(c); return c;
}
async function migration(c, folder) {
  const dir = path.join(repo, 'hrms-database', 'changelog', 'migrations', folder);
  const files = fs.readdirSync(dir).filter(n => n.endsWith('.xml'));
  assert.equal(files.length, 1, `one migration XML required in ${folder}`);
  const xml = fs.readFileSync(path.join(dir, files[0]), 'utf8').replace(/<rollback>[\s\S]*?<\/rollback>/g, '');
  const blocks = [...xml.matchAll(/<sql\s+splitStatements="false">\s*<!\[CDATA\[([\s\S]*?)\]\]>\s*<\/sql>/g)];
  assert(blocks.length > 0, `SQL blocks required in ${folder}`);
  for (const [, sql] of blocks) await c.query(sql.replaceAll('${schema}', 'tenant_fixture'));
}
function trust(entry, tenants) { return { kid: entry.kid, service: entry.service, tenant_ids: tenants, public_key_file: entry.key.publicFile }; }
function isolatedEnvironment(env){
  const base={};for(const [name,value]of Object.entries(process.env))if(/^(PATH|SYSTEMROOT|WINDIR|SYSTEMDRIVE|TEMP|TMP|USERPROFILE|APPDATA|LOCALAPPDATA|PROGRAMDATA|PROGRAMFILES|PROGRAMFILES\(X86\))$/i.test(name))base[name]=value;
  return {...base,RUST_LOG:'warn',KABIPAY_LOG_FORMAT:'json',...env};
}
function start(binary, env, label, args = []) {
  assert(fs.existsSync(binary), `${label} binary missing: ${binary}`);
  const child = spawn(binary, args, { cwd: root, env: isolatedEnvironment(env), windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let log = '';
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { log = (log + chunk.toString()).slice(-10000); });
  children.push({ child, label, readLog: () => log });
  return child;
}
async function ready(url, label) {
  for (let n = 0; n < 100; n++) {
    const dead = children.find(x => x.label === label && x.child.exitCode !== null);
    if (dead) throw Error(`${label} exited: ${dead.readLog()}`);
    try { const r = await fetch(url, { signal: AbortSignal.timeout(500) }); if (r.ok) return; } catch { /* startup */ }
    await delay(150);
  }
  throw Error(`${label} did not become ready: ${children.find(x => x.label === label)?.readLog()}`);
}
async function listening(number, label) {
  for (let n = 0; n < 100; n++) {
    const dead = children.find(x => x.label === label && x.child.exitCode !== null);
    if (dead) throw Error(`${label} exited: ${dead.readLog()}`);
    const open = await new Promise(resolve => {
      const socket = net.connect(number, '127.0.0.1');
      socket.once('connect', () => { socket.destroy(); resolve(true); });
      socket.once('error', () => resolve(false));
    });
    if (open) return;
    await delay(150);
  }
  throw Error(`${label} did not listen: ${children.find(x => x.label === label)?.readLog()}`);
}
async function gql(base, company, token, query, variables = {}, expectError = false) {
  const r = await fetch(`${base}/graphql`, { method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${token}`, 'x-login-company-id': company }, body: JSON.stringify({ query, variables }), signal: AbortSignal.timeout(45000) });
  const raw = await r.text();
  let body; try { body = JSON.parse(raw); } catch { body = { httpBody: raw.slice(0, 500) }; }
  if (expectError) return { status: r.status, body };
  assert.equal(r.status, 200, `GraphQL HTTP ${r.status}: ${JSON.stringify(body)}`);
  assert(!body.errors?.length, `GraphQL error: ${JSON.stringify(body.errors)}`);
  return body.data;
}
async function denied(label, task) {
  const r = await task();
  const rejected = Object.values(r.body?.data || {}).some(x => x?.status === 'REJECTED');
  assert(r.status >= 400 || r.body?.errors?.length || rejected, `${label} unexpectedly succeeded: ${JSON.stringify(r.body)}`);
  console.log(`PASS ${label}`);
}
async function seedOps(ops) {
  for (const [tenant, name, database] of [[ids.a, 'Fixture A', databases[1]], [ids.b, 'Fixture B', databases[2]]]) {
    await ops.query("INSERT INTO kabipay_ops.tenant(id,name,status,is_deleted,created_at,updated_at) VALUES($1,$2,'ACTIVE',false,NOW(),NOW())", [tenant, name]);
    await ops.query("INSERT INTO kabipay_ops.tenant_database(id,tenant_id,db_type,db_host,db_name,schema_name,is_active,created_at,updated_at) VALUES($1,$2,'POSTGRES','127.0.0.1',$3,'tenant_fixture',true,NOW(),NOW())", [randomUUID(), tenant, database]);
  }
  const employeeModule = randomUUID();
  await ops.query("INSERT INTO kabipay_ops.module(id,code,name,is_active,display_order,is_core,created_at,updated_at) VALUES($1,'EMPLOYEE','Employee',true,0,true,NOW(),NOW())", [employeeModule]);
  const module = randomUUID();
  await ops.query("INSERT INTO kabipay_ops.module(id,code,name,is_active,display_order,is_core,created_at,updated_at) VALUES($1,'LEAVE','Leave',true,1,false,NOW(),NOW())", [module]);
  await ops.query("INSERT INTO kabipay_ops.tenant_subscription(id,tenant_id,module_id,status,contracted_seats,current_seat_usage,overage_policy,is_deleted,created_at,updated_at) VALUES($1,$2,$3,'ACTIVE',10,1,'BLOCK',false,NOW(),NOW())", [randomUUID(), ids.b, module]);
  for (const code of ['PAYROLL', 'WORKFLOW']) {
    const id = randomUUID();
    await ops.query('INSERT INTO kabipay_ops.module(id,code,name,is_active,display_order,is_core,created_at,updated_at) VALUES($1,$2,$2,true,2,false,NOW(),NOW())', [id, code]);
    await ops.query("INSERT INTO kabipay_ops.tenant_subscription(id,tenant_id,module_id,status,contracted_seats,current_seat_usage,overage_policy,is_deleted,created_at,updated_at) VALUES($1,$2,$3,'ACTIVE',10,1,'BLOCK',false,NOW(),NOW())", [randomUUID(), ids.b, id]);
  }
  await ops.query("UPDATE kabipay_ops.tenant SET timezone='Asia/Kolkata',currency='INR' WHERE id=ANY($1::uuid[])", [[ids.a,ids.b]]);
  return module;
}
async function seedCompany(c, company, user, employee, module, owner) {
  await c.query("INSERT INTO tenant_fixture.\"user\"(id,tenant_id,username,password_hash,must_change_password,is_active,mfa_enabled,is_deleted,created_at,updated_at) VALUES($1,$2,$3,'synthetic',false,true,false,false,NOW(),NOW())", [user, company, owner ? 'Manager A' : 'Employee B']);
  await c.query("INSERT INTO tenant_fixture.employee(id,tenant_id,user_id,employee_code,first_name,last_name,status,date_of_joining,is_deleted,created_at,updated_at) VALUES($1,$2,$3,$4,$5,'Fixture','ACTIVE','2025-01-01',false,NOW(),NOW())", [employee, company, user, owner ? 'A001' : 'B001', owner ? 'Manager' : 'Employee']);
  const role = randomUUID();
  await c.query("INSERT INTO tenant_fixture.role(id,tenant_id,name,is_system_role,is_deleted,created_at,updated_at) VALUES($1,$2,'Fixture owner',false,false,NOW(),NOW())", [role, company]);
  await c.query("INSERT INTO tenant_fixture.user_role(user_id,role_id,assigned_at) VALUES($1,$2,NOW())", [user, role]);
  const permission = randomUUID();
  await c.query("INSERT INTO tenant_fixture.permission(id,resource,action,module_id,created_at,updated_at) VALUES($1,'role','manage',$2,NOW(),NOW())", [permission, module]);
  await c.query("INSERT INTO tenant_fixture.role_permission(role_id,permission_id,created_at) VALUES($1,$2,NOW())", [role, permission]);
  await c.query("INSERT INTO tenant_fixture.permission_scope(id,tenant_id,role_id,resource,action,scope_type,created_at) VALUES($1,$2,$3,'role','manage','ALL',NOW())", [randomUUID(), company, role]);
}
async function seedLeave(b) {
  const date = new Date(); date.setUTCDate(date.getUTCDate() + 7); const day = date.toISOString().slice(0, 10);
  await b.query("INSERT INTO tenant_fixture.leave_type(id,tenant_id,name,code,is_paid,carry_forward,sandwich_rule,half_day_allowed,requires_document,is_deleted,created_at,updated_at) VALUES($1,$2,'Unpaid leave','LWP',false,false,false,false,false,false,NOW(),NOW())", [ids.leaveType, ids.b]);
  await b.query("INSERT INTO tenant_fixture.workflow(id,tenant_id,name,entity_type,is_active,created_at,updated_at) VALUES($1,$2,'Fixture leave','LEAVE_REQUEST',true,NOW(),NOW())", [ids.workflow, ids.b]);
  await b.query("INSERT INTO tenant_fixture.workflow_step(id,tenant_id,workflow_id,sequence_order,step_name,approver_type,can_skip,created_at,updated_at) VALUES($1,$2,$3,1,'Reporting manager','REPORTING_MANAGER',false,NOW(),NOW())", [ids.step, ids.b, ids.workflow]);
  await b.query("INSERT INTO tenant_fixture.workflow_instance(id,tenant_id,workflow_id,entity_type,entity_id,status,current_step_id,created_at,updated_at) VALUES($1,$2,$3,'LEAVE_REQUEST',$4,'IN_PROGRESS',$5,NOW(),NOW())", [ids.instance, ids.b, ids.workflow, ids.leave, ids.step]);
  await b.query("INSERT INTO tenant_fixture.leave_request(id,tenant_id,employee_id,leave_type_id,from_date,to_date,days_requested,is_half_day,status,reason,workflow_instance_id,uses_comp_off,applied_at,is_deleted,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$5,1,false,'PENDING','Fixture personal leave',$6,false,NOW(),false,NOW(),NOW())", [ids.leave, ids.b, ids.bEmployee, ids.leaveType, day, ids.instance]);
  await b.query("INSERT INTO tenant_fixture.leave_working_date_snapshot(leave_request_id,tenant_id,employee_id,from_date,to_date,requested_days,date_units,calendar_provenance,created_at) VALUES($1,$2,$3,$4,$4,1,$5,$6,NOW())", [ids.leave, ids.b, ids.bEmployee, day, JSON.stringify([{ date: day, units: '1' }]), JSON.stringify({ fixture: true })]);
}

async function seedAdministrationAccounts(b) {
  ids.department=randomUUID();ids.designation=randomUUID();
  await b.query("INSERT INTO tenant_fixture.department(id,tenant_id,name,code,is_deleted,created_at,updated_at) VALUES($1,$2,'Source fixture department','FIXTURE',false,NOW(),NOW())",[ids.department,ids.b]);
  await b.query("INSERT INTO tenant_fixture.designation(id,tenant_id,department_id,title,is_deleted,created_at,updated_at) VALUES($1,$2,$3,'Source fixture engineer',false,NOW(),NOW())",[ids.designation,ids.b,ids.department]);
  await b.query("UPDATE tenant_fixture.employee SET department_id=$3,designation_id=$4,employment_type='PERMANENT' WHERE tenant_id=$1 AND id=$2",[ids.b,ids.bEmployee,ids.department,ids.designation]);
  await b.query("INSERT INTO tenant_fixture.role(id,tenant_id,name,is_system_role,is_deleted,created_at,updated_at) VALUES($1,$3,'Fixture eligible administrator',false,false,NOW(),NOW()),($2,$3,'Fixture staff',false,false,NOW(),NOW())", [ids.adminRole, ids.staffRole, ids.b]);
  const permission=(await b.query("SELECT id FROM tenant_fixture.permission WHERE resource='role' AND action='manage' LIMIT 1")).rows[0].id;
  await b.query('INSERT INTO tenant_fixture.role_permission(role_id,permission_id,created_at) VALUES($1,$2,NOW())',[ids.adminRole,permission]);
  await b.query("INSERT INTO tenant_fixture.permission_scope(id,tenant_id,role_id,resource,action,scope_type,created_at) VALUES($1,$2,$3,'role','manage','ALL',NOW())",[randomUUID(),ids.b,ids.adminRole]);
  await b.query("INSERT INTO tenant_fixture.\"user\"(id,tenant_id,username,password_hash,must_change_password,is_active,mfa_enabled,is_deleted,created_at,updated_at) VALUES($1,$2,'Fixture target logical self','synthetic',false,true,false,false,NOW(),NOW())",[ids.selfAdmin,ids.b]);
  await b.query('INSERT INTO tenant_fixture.user_role(user_id,role_id,assigned_at) VALUES($1,$2,NOW())',[ids.selfAdmin,ids.adminRole]);
}

async function seedReviewedPayroll(b) {
  await b.query('UPDATE tenant_fixture.employee SET payroll_excluded=(id<>$2) WHERE tenant_id=$1',[ids.b,ids.bEmployee]);
  await b.query("INSERT INTO tenant_fixture.salary_component(id,tenant_id,name,code,type,is_taxable,is_fixed,is_active,created_at,updated_at) VALUES($1,$2,'Basic','BASIC','EARNING',true,true,true,NOW(),NOW())",[randomUUID(),ids.b]);
  const input={year:2026,month:9,gross_rule:'FIXED_MINUS_LWP',fixed_gross:'1000.00',earned_gross_override:null,month_days:'30',paid_days:'30',present_days:'30',lwp_days:'0',source_lwp_days:'0',lwp_divisor:'30',lwp_amount_override:null,lwp_basis:'GROSS',lwp_handling:'SOURCE_GROSS_INCLUDES_REDUCTION',variable_allowance_ot:'0',incentive:'0',advance_already_paid:'0',additional_deductions:[],statutory_overrides:{PF:'0',ESI:'0',PT:'0',TDS:'0'},expected_earned_components:{BASIC:'1000.00'},expected_wages:{},expected_employer_contributions:{},expected_statement:{earned_gross:'1000.00',net_earned:'1000.00',remaining_payable:'1000.00'},contribution_rules:{},ready:true,status:'REVIEWED',historical_lwp_included:false,approved_lwp_review_hash:null};
  await b.query('INSERT INTO tenant_fixture.payroll_period_input(id,tenant_id,employee_id,year,month,input,ready,revision,updated_by,created_at,updated_at) VALUES($1,$2,$3,2026,9,$4,true,1,$5,NOW(),NOW())',[randomUUID(),ids.b,ids.bEmployee,input,ids.bUser]);
  await b.query("INSERT INTO tenant_fixture.payroll_cycle(id,tenant_id,name,month,year,status,created_at,updated_at) VALUES($1,$2,'Synthetic reviewed September',9,2026,'DRAFT',NOW(),NOW())",[ids.payrollCycle,ids.b]);
}

async function celebrationManagerBoundary(client) {
  const source=fs.readFileSync(path.join(root,'crates','kabipay-notification','src','services','automated_events.rs'),'utf8');
  const body=source.slice(source.indexOf('async fn load_eligible_employees('));
  const sql=body.match(/r#"(SELECT e\.id AS employee_id[\s\S]*?)"#/)[1];
  const [company,subject,legacy,local,subjectUser,legacyUser,localUser,assignment]=Array.from({length:8},()=>randomUUID());
  await client.query('BEGIN');
  try {
    await client.query(`CREATE TEMP TABLE employee(id uuid,tenant_id uuid,user_id uuid,reporting_manager_id uuid,employee_code text,first_name text,last_name text,date_of_birth date,date_of_joining date,status text,is_deleted boolean) ON COMMIT DROP;
      CREATE TEMP TABLE "user"(id uuid,tenant_id uuid,is_active boolean,is_deleted boolean) ON COMMIT DROP;
      CREATE TEMP TABLE management_assignment(id uuid,tenant_id uuid,subject_employee_id uuid,manager_employee_id uuid,relationship_type text,status text,effective_from timestamptz,effective_to timestamptz) ON COMMIT DROP;
      CREATE TEMP TABLE employee_celebration_preference(tenant_id uuid,employee_id uuid,share_birthday boolean,share_work_anniversary boolean) ON COMMIT DROP;
      SET LOCAL search_path=pg_temp`);
    for(const [employee,user]of [[subject,subjectUser],[legacy,legacyUser],[local,localUser]]){
      await client.query('INSERT INTO "user" VALUES($1,$2,true,false)',[user,company]);
      await client.query("INSERT INTO employee VALUES($1,$2,$3,$4,'FIXTURE','Synthetic','Employee',NULL,CURRENT_DATE,'ACTIVE',false)",[employee,company,user,employee===subject?legacy:null]);
    }
    const manager=async()=>(await client.query(sql,[company])).rows.find(x=>x.employee_id===subject).manager_user_id;
    assert.equal(await manager(),legacyUser,'unmanaged employee retains actual local manager notification');
    await client.query("INSERT INTO management_assignment VALUES($1,$2,$3,NULL,'PRIMARY','ACTIVE',NOW()-INTERVAL '1 minute',NULL)",[assignment,company,subject]);
    assert.equal(await manager(),null,'foreign canonical manager must suppress former local recipient');
    await client.query('UPDATE management_assignment SET manager_employee_id=$1 WHERE id=$2',[local,assignment]);
    assert.equal(await manager(),localUser,'current canonical local manager receives manager notification');
    await client.query("UPDATE management_assignment SET status='ENDED' WHERE id=$1",[assignment]);
    assert.equal(await manager(),legacyUser,'ended canonical assignment restores real local baseline');
  } finally { await client.query('ROLLBACK'); }
  console.log('PASS actual notification recipient SQL: legacy, foreign suppression, local canonical and ended fallback');
}

async function main() {
  fs.mkdirSync(temp, { recursive: true });
  const keys = { clientA: key('client-a'), clientB: key('client-b'), delegation: key('delegation'), orgService: key('service-org'), aEmployee: key('service-a-employee'), bEmployee: key('service-b-employee'), bLeave: key('service-b-leave'), bPayroll:key('service-b-payroll'),bWorkflow:key('service-b-workflow') };
  const service = [
    { key: keys.orgService, kid: keys.orgService.kid, service: 'kabipay-organization' },
    { key: keys.aEmployee, kid: keys.aEmployee.kid, service: 'kabipay-employee' },
    { key: keys.bEmployee, kid: keys.bEmployee.kid, service: 'kabipay-employee' },
    { key: keys.bLeave, kid: keys.bLeave.kid, service: 'kabipay-leave' },
    { key: keys.bPayroll, kid: keys.bPayroll.kid, service: 'kabipay-payroll' },
    { key: keys.bWorkflow, kid: keys.bWorkflow.kid, service: 'kabipay-workflow' },
  ];
  const clientKeys = JSON.stringify([{ kid: keys.clientA.kid, tenant_ids: [ids.a], public_key_file: keys.clientA.publicFile }, { kid: keys.clientB.kid, tenant_ids: [ids.b], public_key_file: keys.clientB.publicFile }]);
  docker(['run', '--pull=never', '--rm', '-d', '--name', container, '--memory', '512m', '--cpus', '2', '-p', '127.0.0.1::5432', '-e', 'POSTGRES_USER=organization_fixture', '-e', 'POSTGRES_PASSWORD=synthetic', '-e', 'POSTGRES_DB=postgres', 'postgres:16-alpine']); created = true;
  pgPort = Number(docker(['port', container, '5432/tcp']).match(/^127\.0\.0\.1:(\d+)$/)?.[1]); assert(pgPort > 0);
  for (let n = 0; n < 100; n++) { if (spawnSync('docker', ['exec', container, 'pg_isready', '-h', '127.0.0.1', '-U', 'organization_fixture'], { stdio: 'ignore', windowsHide: true }).status === 0) break; await delay(150); }
  const admin = new Client({ connectionString: `postgres://organization_fixture:synthetic@127.0.0.1:${pgPort}/postgres` }); await admin.connect(); clients.push(admin);
  for (const name of databases) await admin.query(`CREATE DATABASE ${name}`);
  const ops = await db(databases[0]), a = await db(databases[1]), b = await db(databases[2]);
  await ops.query(fs.readFileSync(path.join(fixtures, 'ops_schema.sql'), 'utf8'));
  for (const folder of ['0111_organization_foundation', '0113_organization_control_outbox', '0114_organization_execution', '0116_organization_projections','0120_organization_capture_lifecycle','0123_organization_job_authority']) await migration(ops, folder);
  for (const c of [a, b]) {
    await c.query(fs.readFileSync(path.join(fixtures, 'source_schema.sql'), 'utf8'));
    // Generated entity tables omit legacy ALTER-only profile columns/defaults.
    // Apply their actual source migration statements inside this empty fixture.
    const profile=fs.readFileSync(path.join(repo,'hrms-database','changelog','migrations','0092_tenant_import_tracking','tenant_import_tracking.xml'),'utf8').split('<![CDATA[')[1].split(']]>')[0];
    for(const sql of profile.split(';').filter(s=>/^\s*ALTER TABLE.*\.(?:employee|employee_bank) ADD COLUMN/.test(s)))await c.query(sql.replaceAll('${schema}','tenant_fixture'));
    await c.query('DROP TABLE tenant_fixture.payroll_draft_calculation');
    await migration(c,'0094_payroll_draft_calculation');
    for (const folder of ['0112_company_management_foundation', '0115_organization_tenant_runtime', '0117_organization_membership_lifecycle','0118_organization_hr_administration','0119_organization_payroll_jobs','0121_organization_source_history']) await migration(c, folder);
  }
  const module = await seedOps(ops);
  await celebrationManagerBoundary(a);
  await seedCompany(a, ids.a, ids.aUser, ids.aEmployee, module, true);
  await seedCompany(b, ids.b, ids.bUser, ids.bEmployee, module, false);
  await seedAdministrationAccounts(b);
  await seedLeave(b);
  console.log('PASS disposable PostgreSQL16, three separate databases, actual source0118/0119/0121 and ops0120/0123 SQL with synthetic business rows');

  const orgPort = await port(), aPort = await port(), bEmployeePort = await port(), bLeavePort = await port(),bPayrollPort=await port(),bWorkflowPort=await port();
  const orgBase = `http://127.0.0.1:${orgPort}`;
  // Explicit transport-failure seam: the organization service reaches its fixed
  // source endpoint, but one decision is dropped before the actual source sees it.
  leaveProxy=http.createServer(async(req,res)=>{try{const chunks=[];let size=0;for await(const chunk of req){size+=chunk.length;if(size>65536)throw Error('fixture body bound');chunks.push(chunk);}const body=Buffer.concat(chunks);if(dropNextLeaveDecision&&JSON.parse(body).operation==='leave_decision'){dropNextLeaveDecision=false;res.writeHead(503);res.end();return;}const result=await fetch(`http://127.0.0.1:${bLeavePort}${req.url}`,{method:'POST',headers:{authorization:req.headers.authorization,'content-type':'application/json'},body,signal:AbortSignal.timeout(40000)});res.writeHead(result.status,{'content-type':'application/json'});res.end(Buffer.from(await result.arrayBuffer()));}catch{res.writeHead(503);res.end();}});
  await new Promise(resolve=>leaveProxy.listen(0,'127.0.0.1',resolve));
  const leaveProxyPort=leaveProxy.address().port;
  const sourceBinary = path.join(root, 'target', 'debug', 'examples', 'organization_fixture_source.exe');
  const orgBinary = path.join(root, 'target', 'debug', 'kabipay-organization.exe');
  const bootstrapBinary = path.join(root, 'target', 'debug', 'bootstrap_organization.exe');
  const identityEndpoints = JSON.stringify({ [ids.a]: `http://127.0.0.1:${aPort}/auth/client/organization-identity`, [ids.b]: `http://127.0.0.1:${bEmployeePort}/auth/client/organization-identity` });
  reportingAcceptance=await prepareOrganizationReportingAcceptance({opsUrl:dbUrl(databases[0]),temp,serviceKey:keys.orgService,tenants:[ids.a,ids.b]});
  const orgEnv = {
    ...reportingAcceptance.environment,
    KABIPAY_ORGANIZATION_DATABASE_URL: dbUrl(databases[0]), KABIPAY_ORGANIZATION_IDENTITY_ENDPOINTS: identityEndpoints,
    KABIPAY_ORGANIZATION_EXECUTION_ENABLED: 'true', KABIPAY_ORGANIZATION_PORT: String(orgPort),
    KABIPAY_ORGANIZATION_RUNTIME_ENDPOINTS: JSON.stringify({ [ids.a]: { 'kabipay-employee': `http://127.0.0.1:${aPort}/internal/organization/command` }, [ids.b]: { 'kabipay-employee': `http://127.0.0.1:${bEmployeePort}/internal/organization/command`, 'kabipay-leave': `http://127.0.0.1:${leaveProxyPort}/internal/organization/command`,'kabipay-payroll':`http://127.0.0.1:${bPayrollPort}/internal/organization/command`,'kabipay-workflow':`http://127.0.0.1:${bWorkflowPort}/internal/organization/command` } }),
    KABIPAY_ORGANIZATION_DELEGATION_KEY_ID: keys.delegation.kid, KABIPAY_ORGANIZATION_DELEGATION_PRIVATE_KEY_FILE: keys.delegation.privateFile,
    KABIPAY_ORGANIZATION_DELEGATION_PUBLIC_KEYS: JSON.stringify({ [keys.delegation.kid]: keys.delegation.publicFile }),
    KABIPAY_SERVICE_ID: 'kabipay-organization', KABIPAY_SERVICE_KEY_ID: keys.orgService.kid, KABIPAY_SERVICE_PRIVATE_KEY_FILE: keys.orgService.privateFile,
    KABIPAY_SERVICE_TRUST_KEYS: JSON.stringify([trust(service[1], [ids.a]), trust(service[2], [ids.b]), trust(service[3], [ids.b]),trust(service[4],[ids.b]),trust(service[5],[ids.b])]),
  };
  function sourceEnv(company, database, serviceName, endpointPort, signer) {
    return {
      KABIPAY_ORGANIZATION_FIXTURE: '1', KABIPAY_FIXTURE_OPS_DATABASE_URL: dbUrl(databases[0]),
      KABIPAY_BOUND_TENANT_ID: company, KABIPAY_BOUND_DATABASE_URL: dbUrl(database), KABIPAY_BOUND_SCHEMA: 'tenant_fixture',
      KABIPAY_FIXTURE_SERVICE: serviceName, KABIPAY_FIXTURE_PORT: String(endpointPort),
      KABIPAY_CLIENT_JWT_MODE: 'rs256', KABIPAY_CLIENT_JWT_PUBLIC_KEYS: JSON.stringify([JSON.parse(clientKeys)[company === ids.a ? 0 : 1]]),
      KABIPAY_ORGANIZATION_POLICY_URL: orgBase,
      KABIPAY_ORGANIZATION_DELEGATION_PUBLIC_KEYS: JSON.stringify({ [keys.delegation.kid]: keys.delegation.publicFile }),
      KABIPAY_SERVICE_ID: `kabipay-${serviceName}`, KABIPAY_SERVICE_KEY_ID: signer.kid, KABIPAY_SERVICE_PRIVATE_KEY_FILE: signer.privateFile,
      KABIPAY_SERVICE_TRUST_KEYS: JSON.stringify([trust(service[0], [company])]),
      KABIPAY_JWT_SECRET: 'fixture-only-unused-rsa-mode',
    };
  }
  start(sourceBinary, { ...sourceEnv(ids.a, databases[1], 'employee', aPort, keys.aEmployee), KABIPAY_CLIENT_JWT_KEY_ID: keys.clientA.kid, KABIPAY_CLIENT_JWT_PRIVATE_KEY_FILE: keys.clientA.privateFile }, 'A employee');
  start(sourceBinary, { ...sourceEnv(ids.b, databases[2], 'employee', bEmployeePort, keys.bEmployee), KABIPAY_CLIENT_JWT_KEY_ID: keys.clientB.kid, KABIPAY_CLIENT_JWT_PRIVATE_KEY_FILE: keys.clientB.privateFile }, 'B employee');
  start(sourceBinary, { ...sourceEnv(ids.b, databases[2], 'leave', bLeavePort, keys.bLeave), KABIPAY_CLIENT_JWT_KEY_ID: keys.clientB.kid, KABIPAY_CLIENT_JWT_PRIVATE_KEY_FILE: keys.clientB.privateFile }, 'B leave');
  const payrollEnvironment=sourceEnv(ids.b,databases[2],'payroll',bPayrollPort,keys.bPayroll);
  let payrollChild=start(sourceBinary,payrollEnvironment,'B payroll');
  start(sourceBinary,sourceEnv(ids.b,databases[2],'workflow',bWorkflowPort,keys.bWorkflow),'B workflow');
  start(orgBinary, orgEnv, 'organization');
  await Promise.all([ready(`${orgBase}/healthz`, 'organization'), listening(aPort, 'A employee'), listening(bEmployeePort, 'B employee'), listening(bLeavePort, 'B leave'),listening(bPayrollPort,'B payroll'),listening(bWorkflowPort,'B workflow')]);
  async function restartPayroll(){const done=new Promise(resolve=>payrollChild.once('exit',resolve));payrollChild.kill();await done;payrollChild=start(sourceBinary,payrollEnvironment,`B payroll restart ${children.length}`);await listening(bPayrollPort,children.at(-1).label);}
  const tokenA = jwt(ids.a, ids.aUser, keys.clientA), tokenB = jwt(ids.b, ids.bUser, keys.clientB);
  for (const [label, endpoint, token] of [['A', aPort, tokenA], ['B', bEmployeePort, tokenB]]) {
    const check = await fetch(`http://127.0.0.1:${endpoint}/auth/client/organization-identity`, { method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${token}` }, body: JSON.stringify({ nonce: randomUUID() }) });
    assert.equal(check.status, 200, `${label} real Auth identity failed ${check.status}: ${(await check.text()).slice(0, 500)}; ${children.find(x => x.label === `${label} employee`)?.readLog()}`);
  }
  const bootstrap = spawnSync(bootstrapBinary, [ids.a, 'Fixture Organization'], { cwd: root, env: isolatedEnvironment({ ...orgEnv, KABIPAY_ORGANIZATION_BOOTSTRAP_TOKEN: tokenA }), encoding: 'utf8', timeout: 60000, windowsHide: true });
  assert.equal(bootstrap.status, 0, `bootstrap failed: ${bootstrap.stderr}`);
  const org = bootstrap.stdout.match(/organization_id=([a-f0-9-]+)/)?.[1]; assert(org);
  let version = 1;
  const memberships = await ops.query('SELECT id,tenant_id,version FROM kabipay_ops.organization_company WHERE organization_id=$1', [org]);
  const aMembership = memberships.rows.find(x => x.tenant_id === ids.a)?.id; assert(aMembership);
  console.log('PASS real Auth identity and operator bootstrap');

  const register = 'mutation { registerOrganizationIdentity }';
  await gql(orgBase, ids.b, tokenB, register);
  const invite = await gql(orgBase, ids.a, tokenA, 'mutation($o:UUID!,$v:Int!,$t:UUID!){ inviteOrganizationCompany(organizationId:$o,expectedVersion:$v,tenantId:$t){id status version organizationVersion} }', { o: org, v: version, t: ids.b });
  const bMembership = invite.inviteOrganizationCompany.id; version = invite.inviteOrganizationCompany.organizationVersion;
  const accept = await gql(orgBase, ids.b, tokenB, 'mutation($o:UUID!,$v:Int!,$m:UUID!){ acceptOrganizationCompany(organizationId:$o,expectedVersion:$v,membershipId:$m,expectedMembershipVersion:1){id status version organizationVersion} }', { o: org, v: version, m: bMembership });
  version = accept.acceptOrganizationCompany.organizationVersion;
  assert.equal(accept.acceptOrganizationCompany.status, 'ACTIVE');
  console.log('PASS invitation and source-company acceptance');

  const installMembership = 'mutation($o:UUID!,$m:UUID!,$v:Int!,$c:UUID!){ installOrganizationMembership(organizationId:$o,membershipId:$m,expectedVersion:$v,commandId:$c){status resourceVersion result} }';
  const membershipCommands = new Map();
  for (const [m, v] of [[aMembership, 1], [bMembership, 2]]) {
    const command = randomUUID();
    membershipCommands.set(m, command);
    const x = await gql(orgBase, ids.a, tokenA, installMembership, { o: org, m, v, c: command });
    assert.equal(x.installOrganizationMembership.status, 'COMMITTED', `membership ${m}: ${JSON.stringify(x.installOrganizationMembership)}`);
  }
  console.log('PASS independently installed A and B membership fences');

  // Deliberate fixture seam: erase only central ACK fields after the real source
  // commit, preserving the exact original intent and source receipt. This
  // simulates central uncertainty and tests the real status/receipt/ACK route.
  await ops.query("UPDATE kabipay_ops.organization_company SET installed_version=NULL,installed_hash=NULL,runtime_status='UNINSTALLED' WHERE id=$1 AND organization_id=$2", [bMembership, org]);
  await ops.query("UPDATE kabipay_ops.organization_command_intent SET status='AWAITING_CONFIRMATION' WHERE command_id=$1 AND operation='install_membership'", [membershipCommands.get(bMembership)]);
  assert.equal((await b.query('SELECT status FROM tenant_fixture.organization_membership_installation WHERE membership_id=$1', [bMembership])).rows[0].status, 'ACTIVE');
  const recovered = await gql(orgBase, ids.a, tokenA, 'query($c:UUID!){ organizationCommandStatus(commandId:$c){status resourceVersion result} }', { c: membershipCommands.get(bMembership) });
  assert.equal(recovered.organizationCommandStatus.status, 'COMMITTED', JSON.stringify(recovered));
  const acknowledged = (await ops.query('SELECT installed_version,runtime_status FROM kabipay_ops.organization_company WHERE id=$1', [bMembership])).rows[0];
  assert.equal(Number(acknowledged.installed_version), 2, 'status read must restore central ACK');
  assert.equal(acknowledged.runtime_status, 'ACTIVE');
  console.log('PASS simulated lost membership ACK recovered through real command status and source receipt');

  const principalFor=async(company,user)=>(await ops.query("SELECT principal_id FROM kabipay_ops.principal_login_link WHERE source_tenant_id=$1 AND source_user_id=$2 AND status='VERIFIED'",[company,user])).rows[0].principal_id;
  const principalA=await principalFor(ids.a,ids.aUser),principalB=await principalFor(ids.b,ids.bUser);
  const createRoleQuery='mutation($o:UUID!,$v:Int!,$n:String!,$c:[RoleCapability!]!){createOrganizationRole(organizationId:$o,expectedVersion:$v,name:$n,capabilities:$c){id version status organizationVersion}}';
  const grantRoleQuery='mutation($o:UUID!,$v:Int!,$p:UUID!,$r:UUID!,$m:[UUID!]!){grantOrganizationRole(organizationId:$o,expectedVersion:$v,principalId:$p,roleId:$r,companyMembershipIds:$m){id version status organizationVersion}}';
  async function role(company,token,name,codes){const result=(await gql(orgBase,company,token,createRoleQuery,{o:org,v:version,n:name,c:codes.map(code=>({code,employeeScope:'ALL'}))})).createOrganizationRole;version=result.organizationVersion;return result.id;}
  async function grant(company,token,principal,roleId,members){const result=(await gql(orgBase,company,token,grantRoleQuery,{o:org,v:version,p:principal,r:roleId,m:members})).grantOrganizationRole;version=result.organizationVersion;return result;}
  const delegator=await role(ids.a,tokenA,'Fixture independent source delegator',['organization:manage','organization:delegate']);
  assert.equal((await grant(ids.a,tokenA,principalB,delegator,[aMembership,bMembership])).status,'ACTIVE');
  const adminDefinition=await role(ids.b,tokenB,'Fixture explicit HR administration',['employee:manage','role:manage','workflow:manage','onboarding:manage']);
  const administrationGrant=await grant(ids.b,tokenB,principalA,adminDefinition,[bMembership]);
  const payrollDefinition=await role(ids.b,tokenB,'Fixture explicit Payroll',['payroll:manage']);
  const payrollGrant=await grant(ids.b,tokenB,principalA,payrollDefinition,[bMembership]);
  const installGrantQuery='mutation($o:UUID!,$m:UUID!,$b:UUID!,$c:UUID!){installOrganizationGrant(organizationId:$o,membershipId:$m,bindingId:$b,expectedVersion:1,commandId:$c){status result}}';
  for(const binding of [administrationGrant,payrollGrant]){
    assert.equal(binding.status,'PENDING');
    const installed=(await gql(orgBase,ids.b,tokenB,installGrantQuery,{o:org,m:bMembership,b:binding.id,c:randomUUID()})).installOrganizationGrant;
    assert.equal(installed.status,'COMMITTED',JSON.stringify(installed));
    assert.equal((await ops.query('SELECT activation_status FROM kabipay_ops.role_binding_company WHERE binding_id=$1 AND organization_company_id=$2',[binding.id,bMembership])).rows[0].activation_status,'ACTIVE');
  }
  console.log('PASS real independent delegator grants and signed source installation/ACK for separate HR and Payroll authorities');
  const refsQuery='query($o:UUID!,$m:UUID!){organizationResource(organizationId:$o,membershipId:$m,operation:EMPLOYEE_REFERENCES,resourceId:$m,expectedVersion:2){result}}';
  const sourceRefs=(await gql(orgBase,ids.a,tokenA,refsQuery,{o:org,m:bMembership})).organizationResource.result;
  assert(sourceRefs.departments.some(x=>x.id===ids.department));assert(sourceRefs.designations.some(x=>x.id===ids.designation));assert(sourceRefs.statuses.includes('ACTIVE'));assert(sourceRefs.employment_types.includes('PERMANENT'));
  const listQuery='query($o:UUID!,$m:UUID!,$p:JSON!){organizationResource(organizationId:$o,membershipId:$m,operation:EMPLOYEE_LIST,resourceId:$m,expectedVersion:2,payload:$p){result}}';
  const sourceEmployees=(await gql(orgBase,ids.a,tokenA,listQuery,{o:org,m:bMembership,p:{after:null,first:10}})).organizationResource.result;
  assert(sourceEmployees.employees.some(x=>x.employee_id===ids.bEmployee));
  console.log('PASS real employee collection and source department/designation/status/employment references');
  const selfToken=jwt(ids.b,ids.selfAdmin,keys.clientB,null);
  const actualSelf=await fetch(`http://127.0.0.1:${bEmployeePort}/auth/client/organization-identity`,{method:'POST',headers:{'content-type':'application/json',authorization:`Bearer ${selfToken}`},body:JSON.stringify({nonce:randomUUID()})});
  assert.equal(actualSelf.status,200,'actual target self-admin account must authenticate');
  await gql(orgBase,ids.b,selfToken,register);
  // Explicit fixture-only identity-link setup after actual source proof. This
  // creates the cross-company logical-self case; it is no service ACK substitute.
  await ops.query("UPDATE kabipay_ops.principal_login_link SET principal_id=$1 WHERE source_tenant_id=$2 AND source_user_id=$3 AND status='VERIFIED'",[principalA,ids.b,ids.selfAdmin]);
  assert.equal(await principalFor(ids.b,ids.selfAdmin),principalA);
  const revokeQuery='mutation($o:UUID!,$v:Int!,$b:UUID!){revokeOrganizationRoleBinding(organizationId:$o,expectedVersion:$v,bindingId:$b,expectedBindingVersion:1){status organizationVersion}}';
  async function revoke(binding){const result=(await gql(orgBase,ids.b,tokenB,revokeQuery,{o:org,v:version,b:binding.id})).revokeOrganizationRoleBinding;version=result.organizationVersion;assert.equal(result.status,'REVOKED');}

  const sync = 'mutation($o:UUID!,$m:UUID!,$a:UUID){ syncOrganizationDirectory(organizationId:$o,membershipId:$m,after:$a,first:50){items endCursor hasNextPage graphVersion} }';
  for (const membership of [aMembership, bMembership]) {
    let after = null;
    for (let n = 0; n < 10; n++) {
      const x = (await gql(orgBase, ids.a, tokenA, sync, { o: org, m: membership, a: after })).syncOrganizationDirectory;
      if (!x.hasNextPage) break;
      after = x.endCursor; assert(after);
    }
  }
  const refs = await ops.query('SELECT id,source_tenant_id,source_employee_id FROM kabipay_ops.employment_ref WHERE organization_id=$1', [org]);
  const aRef = refs.rows.find(x => x.source_employee_id === ids.aEmployee)?.id, bRef = refs.rows.find(x => x.source_employee_id === ids.bEmployee)?.id;
  assert(aRef && bRef, 'directory must contain both companies');
  console.log('PASS real source directory pages and separate company employment refs');

  const proposed = await gql(orgBase, ids.a, tokenA, 'mutation($o:UUID!,$s:UUID!,$m:UUID!,$from:DateTime!){ proposeOrganizationRelationship(organizationId:$o,expectedGraphVersion:1,subjectEmploymentRefId:$s,managerEmploymentRefId:$m,relationshipType:"PRIMARY",effectiveFrom:$from,capabilities:["employee:read","leave:read","leave:approve"]) }', { o: org, s: bRef, m: aRef, from: new Date(Date.now() - 60000).toISOString() });
  const proposal = proposed.proposeOrganizationRelationship; assert(proposal.id);
  const assignmentCommandId=randomUUID();
  const installed = await gql(orgBase, ids.a, tokenA, 'mutation($o:UUID!,$m:UUID!,$a:UUID!,$c:UUID!){ installOrganizationRelationship(organizationId:$o,membershipId:$m,assignmentId:$a,expectedVersion:1,commandId:$c){status resourceVersion} }', { o: org, m: bMembership, a: proposal.id, c: assignmentCommandId });
  assert.equal(installed.installOrganizationRelationship.status, 'COMMITTED');
  const team = await gql(orgBase, ids.a, tokenA, 'query($o:UUID!){ organizationTeam(organizationId:$o,first:50){items} }', { o: org });
  assert(team.organizationTeam.items.some(x => x.employee_id === ids.bEmployee));
  console.log('PASS foreign PRIMARY manager installation and team visibility');
  const changeManager='mutation($o:UUID!,$a:UUID!,$v:Int!,$g:Int!,$m:UUID!){ changeOrganizationRelationshipManager(organizationId:$o,assignmentId:$a,expectedVersion:$v,expectedGraphVersion:$g,managerEmploymentRefId:$m) }';
  const installAssignment='mutation($o:UUID!,$m:UUID!,$a:UUID!,$v:Int!,$c:UUID!){ installOrganizationRelationship(organizationId:$o,membershipId:$m,assignmentId:$a,expectedVersion:$v,commandId:$c){status resourceVersion} }';
  await ops.query('UPDATE kabipay_ops.organization_relationship SET installed_version=NULL,installed_hash=NULL,installed_configuration=NULL WHERE id=$1',[proposal.id]);
  await ops.query("UPDATE kabipay_ops.organization_command_intent SET status='AWAITING_CONFIRMATION' WHERE command_id=$1",[assignmentCommandId]);
  const blockedChange=await gql(orgBase,ids.a,tokenA,changeManager,{o:org,a:proposal.id,v:1,g:2,m:aRef},true);
  assert.equal(blockedChange.body.errors?.[0]?.extensions?.code,'ORGANIZATION_INSTALLATION_UNCONFIRMED');
  await gql(orgBase,ids.a,tokenA,'query($c:UUID!){ organizationCommandStatus(commandId:$c){status} }',{c:assignmentCommandId});
  assert.equal(Number((await ops.query('SELECT installed_version FROM kabipay_ops.organization_relationship WHERE id=$1',[proposal.id])).rows[0].installed_version),1);
  console.log('PASS unknown source installation reserves graph until exact status ACK');

  const secondEmployee=randomUUID();
  await a.query("INSERT INTO tenant_fixture.employee(id,tenant_id,employee_code,first_name,last_name,status,date_of_joining,is_deleted,created_at,updated_at) VALUES($1,$2,'SECOND','Second','Manager','ACTIVE',CURRENT_DATE-365,false,NOW(),NOW())",[secondEmployee,ids.a]);
  await gql(orgBase,ids.a,tokenA,sync,{o:org,m:aMembership,a:null});
  const secondRef=(await ops.query('SELECT id FROM kabipay_ops.employment_ref WHERE organization_company_id=$1 AND source_employee_id=$2',[aMembership,secondEmployee])).rows[0].id;
  const toSecond=(await gql(orgBase,ids.a,tokenA,changeManager,{o:org,a:proposal.id,v:1,g:2,m:secondRef})).changeOrganizationRelationshipManager;
  assert.equal(toSecond.version,2);
  assert.equal((await gql(orgBase,ids.a,tokenA,installAssignment,{o:org,m:bMembership,a:proposal.id,v:2,c:randomUUID()})).installOrganizationRelationship.status,'COMMITTED');
  const oldManagerTeam=await gql(orgBase,ids.a,tokenA,'query($o:UUID!){ organizationTeam(organizationId:$o,first:50){items} }',{o:org});
  assert(!oldManagerTeam.organizationTeam.items.some(x=>x.employee_id===ids.bEmployee));
  await gql(orgBase,ids.a,tokenA,changeManager,{o:org,a:proposal.id,v:2,g:3,m:aRef});
  assert.equal((await gql(orgBase,ids.a,tokenA,installAssignment,{o:org,m:bMembership,a:proposal.id,v:3,c:randomUUID()})).installOrganizationRelationship.status,'COMMITTED');
  const reverse=(await gql(orgBase,ids.a,tokenA,'mutation($o:UUID!,$s:UUID!,$m:UUID!,$f:DateTime!){proposeOrganizationRelationship(organizationId:$o,expectedGraphVersion:4,subjectEmploymentRefId:$s,managerEmploymentRefId:$m,relationshipType:"PRIMARY",effectiveFrom:$f,capabilities:["employee:read","leave:approve"])}',{o:org,s:secondRef,m:bRef,f:new Date(Date.now()-1000).toISOString()})).proposeOrganizationRelationship;
  assert.equal((await gql(orgBase,ids.a,tokenA,installAssignment,{o:org,m:aMembership,a:reverse.id,v:1,c:randomUUID()})).installOrganizationRelationship.status,'COMMITTED');
  const reverseTeam=await gql(orgBase,ids.b,tokenB,'query($o:UUID!){organizationTeam(organizationId:$o,first:50){items}}',{o:org});
  assert(reverseTeam.organizationTeam.items.some(x=>x.employee_id===secondEmployee&&x.tenant_id===ids.a));
  const cycle=await gql(orgBase,ids.a,tokenA,'mutation($o:UUID!,$s:UUID!,$m:UUID!,$f:DateTime!){proposeOrganizationRelationship(organizationId:$o,expectedGraphVersion:5,subjectEmploymentRefId:$s,managerEmploymentRefId:$m,relationshipType:"PRIMARY",effectiveFrom:$f,capabilities:["employee:read"])}',{o:org,s:aRef,m:secondRef,f:new Date(Date.now()-1000).toISOString()},true);
  assert(cycle.body.errors?.length,'cross-company person cycle must be rejected');
  console.log('PASS atomic manager replacement, previous manager visibility removed, reverse-company reports and cycle denial');


  // The actual Kafka/CDC pipeline has separate acceptance. A current purpose projection
  // is seeded here solely to exercise the central inbox discovery and command routing seam.
  await ops.query("INSERT INTO kabipay_ops.organization_inbox_projection(id,organization_id,source_tenant_id,resource_type,resource_id,employee_id,employee_display_name,status,summary,source_version,occurred_at,deleted,payload_hash) VALUES($1,$2,$3,'leave_request',$4,$5,'Employee Fixture','PENDING','Fixture leave',1,NOW(),false,repeat('a',64))", [randomUUID(), org, ids.b, ids.leave, ids.bEmployee]);
  const approvals = await gql(orgBase, ids.a, tokenA, 'query($o:UUID!){ organizationApprovals(organizationId:$o,first:50){items} }', { o: org });
  assert(approvals.organizationApprovals.items.some(x => x.resource_id === ids.leave));
  const detailQuery = 'query($o:UUID!,$m:UUID!,$r:UUID!,$v:Int!){ organizationResource(organizationId:$o,membershipId:$m,operation:LEAVE_DETAIL,resourceId:$r,expectedVersion:$v){status resourceVersion result} }';
  const detail = await gql(orgBase, ids.a, tokenA, detailQuery, { o: org, m: bMembership, r: ids.leave, v: 1 });
  assert.equal(detail.organizationResource.result.expected_workflow_step_id, ids.step);
  const commandId = randomUUID();
  const decisionQuery = 'mutation($o:UUID!,$m:UUID!,$c:OrganizationCommandInput!){ executeOrganizationCommand(organizationId:$o,membershipId:$m,command:$c){commandId status resourceVersion result} }';
  const command = { commandId, resourceId: ids.leave, expectedVersion: detail.organizationResource.resourceVersion, operation: 'LEAVE_DECISION', payload: { decision: 'approve', expected_workflow_step_id: ids.step, rejection_reason: null } };
  const decision = await gql(orgBase, ids.a, tokenA, decisionQuery, { o: org, m: bMembership, c: command });
  assert.equal(decision.executeOrganizationCommand.status, 'COMMITTED');
  assert.equal(decision.executeOrganizationCommand.result.status, 'APPROVED');
  const replay = await gql(orgBase, ids.a, tokenA, decisionQuery, { o: org, m: bMembership, c: command });
  assert.equal(replay.executeOrganizationCommand.status, 'COMMITTED');
  assert.equal((await b.query('SELECT count(*)::int AS n FROM tenant_fixture.workflow_action WHERE instance_id=$1', [ids.instance])).rows[0].n, 1);
  console.log('PASS purpose projection seam, live detail, unpaid leave approval and exact receipt replay once');

  const cancelQuery='mutation($c:UUID!){ cancelPendingOrganizationCommand(commandId:$c){commandId resourceId status resourceVersion result} }';
  const committedCancel=(await gql(orgBase,ids.a,tokenA,cancelQuery,{c:commandId})).cancelPendingOrganizationCommand;
  assert.equal(committedCancel.status,'COMMITTED','cancellation never undoes a committed command');
  const pendingId=randomUUID(),pendingInstance=randomUUID();const pendingDate=new Date();pendingDate.setUTCDate(pendingDate.getUTCDate()+8);const pendingDay=pendingDate.toISOString().slice(0,10);
  await b.query("INSERT INTO tenant_fixture.workflow_instance(id,tenant_id,workflow_id,entity_type,entity_id,status,current_step_id,created_at,updated_at) VALUES($1,$2,$3,'LEAVE_REQUEST',$4,'IN_PROGRESS',$5,NOW(),NOW())",[pendingInstance,ids.b,ids.workflow,pendingId,ids.step]);
  await b.query("INSERT INTO tenant_fixture.leave_request(id,tenant_id,employee_id,leave_type_id,from_date,to_date,days_requested,is_half_day,status,reason,workflow_instance_id,uses_comp_off,applied_at,is_deleted,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$5,1,false,'PENDING','Synthetic cancellation test',$6,false,NOW(),false,NOW(),NOW())",[pendingId,ids.b,ids.bEmployee,ids.leaveType,pendingDay,pendingInstance]);
  await b.query("INSERT INTO tenant_fixture.leave_working_date_snapshot(leave_request_id,tenant_id,employee_id,from_date,to_date,requested_days,date_units,calendar_provenance,created_at) VALUES($1,$2,$3,$4,$4,1,$5,$6,NOW())",[pendingId,ids.b,ids.bEmployee,pendingDay,JSON.stringify([{date:pendingDay,units:'1'}]),JSON.stringify({fixture:true})]);
  await ops.query("INSERT INTO kabipay_ops.organization_inbox_projection(id,organization_id,source_tenant_id,resource_type,resource_id,employee_id,employee_display_name,status,summary,source_version,occurred_at,deleted,payload_hash) VALUES($1,$2,$3,'leave_request',$4,$5,'Employee Fixture','PENDING','Fixture cancellation',1,NOW(),false,repeat('b',64))",[randomUUID(),org,ids.b,pendingId,ids.bEmployee]);
  const neverDelivered={...command,commandId:randomUUID(),resourceId:pendingId,expectedVersion:1};
  dropNextLeaveDecision=true;
  const unknown=(await gql(orgBase,ids.a,tokenA,decisionQuery,{o:org,m:bMembership,c:neverDelivered})).executeOrganizationCommand;
  assert.equal(unknown.status,'AWAITING_CONFIRMATION');
  assert.equal((await b.query('SELECT count(*)::int AS n FROM tenant_fixture.organization_command_receipt WHERE command_id=$1',[neverDelivered.commandId])).rows[0].n,0);
  await denied('another actor cannot cancel the pending command',()=>gql(orgBase,ids.b,tokenB,cancelQuery,{c:neverDelivered.commandId},true));
  const cancelled=(await gql(orgBase,ids.a,tokenA,cancelQuery,{c:neverDelivered.commandId})).cancelPendingOrganizationCommand;
  assert.equal(cancelled.status,'CANCELLED_NOT_EXECUTED',JSON.stringify(cancelled));assert.equal(cancelled.commandId,neverDelivered.commandId);assert.equal(cancelled.resourceId,pendingId);
  assert.equal((await b.query('SELECT status FROM tenant_fixture.leave_request WHERE id=$1',[pendingId])).rows[0].status,'PENDING');
  const delayed=(await gql(orgBase,ids.a,tokenA,decisionQuery,{o:org,m:bMembership,c:neverDelivered})).executeOrganizationCommand;
  assert.equal(delayed.status,'CANCELLED_NOT_EXECUTED','late original delivery must hit source cancellation receipt');
  const replacement=(await gql(orgBase,ids.a,tokenA,decisionQuery,{o:org,m:bMembership,c:{...neverDelivered,commandId:randomUUID()}})).executeOrganizationCommand;
  assert.equal(replacement.status,'COMMITTED');
  assert.equal((await b.query('SELECT count(*)::int AS n FROM tenant_fixture.workflow_action WHERE instance_id=$1',[pendingInstance])).rows[0].n,1);
  assert.equal((await ops.query("SELECT count(*)::int AS n FROM kabipay_ops.organization_outbox WHERE aggregate_id=$1 AND event_type='command.cancelled'",[neverDelivered.commandId])).rows[0].n,1);
  console.log('PASS source-fenced cancellation, no undo after commit, late-original denial, fresh command once and retained cancellation audit');

  // Simulate a lost response for a committed business action, then replace the
  // exact authority revision through the real API. Receipt disclosure remains
  // bound to its original authority; obsolete recovery references must disappear.
  await ops.query("UPDATE kabipay_ops.organization_command_intent SET status='AWAITING_CONFIRMATION' WHERE command_id=$1",[commandId]);
  const pendingQuery='query($o:UUID!){organizationPendingCommands(organizationId:$o,first:100){items hasNextPage}}';
  const beforeAuthorityChange=(await gql(orgBase,ids.a,tokenA,pendingQuery,{o:org})).organizationPendingCommands;
  assert(beforeAuthorityChange.items.some(x=>x.command_id===commandId),'current exact authority can recover original command');
  await gql(orgBase,ids.a,tokenA,changeManager,{o:org,a:proposal.id,v:3,g:5,m:aRef});
  assert.equal((await gql(orgBase,ids.a,tokenA,installAssignment,{o:org,m:bMembership,a:proposal.id,v:4,c:randomUUID()})).installOrganizationRelationship.status,'COMMITTED');
  const afterAuthorityChange=(await gql(orgBase,ids.a,tokenA,pendingQuery,{o:org})).organizationPendingCommands;
  assert(!afterAuthorityChange.items.some(x=>x.command_id===commandId),'obsolete authority must not restore an unrecoverable command');
  assert.equal((await b.query('SELECT count(*)::int AS n FROM tenant_fixture.workflow_action WHERE instance_id=$1',[ids.instance])).rows[0].n,1,'authority change cannot repeat the committed action');
  const currentDetail=(await gql(orgBase,ids.a,tokenA,detailQuery,{o:org,m:bMembership,r:ids.leave,v:decision.executeOrganizationCommand.resourceVersion})).organizationResource;
  assert.equal(currentDetail.status,'COMMITTED','new authority must load fresh source state');
  console.log('PASS obsolete authority recovery reference removed without exposing old receipt or repeating business action');

  await seedReviewedPayroll(b);
  await runPayrollJobAcceptance({centralUrl:orgBase,sourceUrl:`http://127.0.0.1:${bPayrollPort}`,sourceCompanyId:ids.a,targetCompanyId:ids.b,token:tokenA,organizationId:org,membershipId:bMembership,sourceDb:b,cycleId:ids.payrollCycle,restartSource:restartPayroll,revokeGrant:()=>revoke(payrollGrant),wrongMembershipId:aMembership});
  // The original B bootstrap account stops being an administrator before safety
  // cases, so an unrelated eligible account cannot disguise a last-admin guard.
  await b.query('DELETE FROM tenant_fixture.user_role WHERE user_id=$1',[ids.bUser]);
  await b.query('INSERT INTO tenant_fixture.user_role(user_id,role_id,assigned_at) VALUES($1,$2,NOW())',[ids.bUser,ids.staffRole]);
  await runHrAdministrationAcceptance({centralUrl:orgBase,sourceCompanyId:ids.a,targetCompanyId:ids.b,token:tokenA,organizationId:org,membershipId:bMembership,sourceDb:b,opsDb:ops,employeeId:ids.bEmployee,adminRoleId:ids.adminRole,staffRoleId:ids.staffRole,selfAdminUserId:ids.selfAdmin,beforeAdministrationRevocation:()=>reportingAcceptance.run({ops,gql,centralUrl:orgBase,sourceCompanyId:ids.a,targetCompanyId:ids.b,token:tokenA,organizationId:org,membershipId:bMembership,employeeId:ids.bEmployee}),revokeAdministrationGrant:()=>revoke(administrationGrant)});


  await denied('stale resource version', () => gql(orgBase, ids.a, tokenA, decisionQuery, { o: org, m: bMembership, c: { ...command, commandId: randomUUID(), expectedVersion: 1 } }, true));
  await denied('forged source-company selector', () => gql(orgBase, ids.b, tokenA, 'query { myOrganizationIdentity { id } }', {}, true));
  await a.query('UPDATE tenant_fixture."user" SET is_active=false WHERE id=$1', [ids.aUser]);
  await denied('inactive source actor', () => gql(orgBase, ids.a, tokenA, 'query { myOrganizationIdentity { id } }', {}, true));
  await a.query('UPDATE tenant_fixture."user" SET is_active=true WHERE id=$1', [ids.aUser]);

  const lifecycle = 'mutation($o:UUID!,$v:Int!,$m:UUID!,$mv:Int!){ ACTION(organizationId:$o,expectedVersion:$v,membershipId:$m,expectedMembershipVersion:$mv){status version organizationVersion} }';
  const activePrincipal=(await ops.query('SELECT principal_id FROM kabipay_ops.principal_login_link WHERE source_tenant_id=$1 AND source_user_id=$2',[ids.a,ids.aUser])).rows[0].principal_id;
  await ops.query("UPDATE kabipay_ops.principal SET status='DISABLED',identity_version=identity_version+1 WHERE id=$1",[activePrincipal]);
  await denied('central principal revocation with company still active',()=>gql(orgBase,ids.a,tokenA,detailQuery,{o:org,m:bMembership,r:ids.leave,v:decision.executeOrganizationCommand.resourceVersion},true));
  await ops.query("UPDATE kabipay_ops.principal SET status='ACTIVE',identity_version=identity_version+1 WHERE id=$1",[activePrincipal]);
  const suspend = await gql(orgBase, ids.a, tokenA, lifecycle.replace('ACTION', 'suspendOrganizationCompany'), { o: org, v: version, m: bMembership, mv: 2 });
  version = suspend.suspendOrganizationCompany.organizationVersion;
  assert.equal(suspend.suspendOrganizationCompany.status, 'SUSPENDED');
  const suspended = await gql(orgBase, ids.a, tokenA, installMembership, { o: org, m: bMembership, v: 3, c: randomUUID() });
  assert.equal(suspended.installOrganizationMembership.status, 'COMMITTED', JSON.stringify(suspended));
  assert.equal((await b.query('SELECT status FROM tenant_fixture.organization_membership_installation WHERE membership_id=$1', [bMembership])).rows[0].status, 'SUSPENDED');
  await denied('suspended company command fence', () => gql(orgBase, ids.a, tokenA, detailQuery, { o: org, m: bMembership, r: ids.leave, v: 2 }, true));
  // Cleanup must remain possible when the manager's company is suspended.
  await gql(orgBase,ids.a,tokenA,'mutation($o:UUID!,$a:UUID!){endOrganizationRelationship(organizationId:$o,assignmentId:$a,expectedVersion:1,expectedGraphVersion:6)}',{o:org,a:reverse.id});
  assert.equal((await gql(orgBase,ids.a,tokenA,installAssignment,{o:org,m:aMembership,a:reverse.id,v:2,c:randomUUID()})).installOrganizationRelationship.status,'COMMITTED');
  console.log('PASS end/install a relationship whose manager company is suspended');
  const resume = await gql(orgBase, ids.a, tokenA, lifecycle.replace('ACTION', 'resumeOrganizationCompany'), { o: org, v: version, m: bMembership, mv: 3 });
  version = resume.resumeOrganizationCompany.organizationVersion;
  assert.equal(resume.resumeOrganizationCompany.status, 'ACTIVE');
  const resumed = await gql(orgBase, ids.a, tokenA, installMembership, { o: org, m: bMembership, v: 4, c: randomUUID() });
  assert.equal(resumed.installOrganizationMembership.status, 'COMMITTED', JSON.stringify(resumed));
  assert.equal((await b.query('SELECT status FROM tenant_fixture.organization_membership_installation WHERE membership_id=$1', [bMembership])).rows[0].status, 'ACTIVE');
  const resumedTeam = await gql(orgBase, ids.a, tokenA, 'query($o:UUID!){ organizationTeam(organizationId:$o,first:50){items} }', { o: org });
  assert(resumedTeam.organizationTeam.items.some(x => x.employee_id === ids.bEmployee));
  const detach = await gql(orgBase, ids.a, tokenA, lifecycle.replace('ACTION', 'beginOrganizationCompanyDetach'), { o: org, v: version, m: bMembership, mv: 4 });
  version = detach.beginOrganizationCompanyDetach.organizationVersion;
  assert.equal(detach.beginOrganizationCompanyDetach.status, 'DETACHING');
  const detached = await gql(orgBase, ids.a, tokenA, installMembership, { o: org, m: bMembership, v: 5, c: randomUUID() });
  assert.equal(detached.installOrganizationMembership.status, 'COMMITTED', JSON.stringify(detached));
  assert.equal((await b.query('SELECT status FROM tenant_fixture.organization_membership_installation WHERE membership_id=$1', [bMembership])).rows[0].status, 'DETACHED');
  assert.equal((await b.query("SELECT count(*)::int AS n FROM tenant_fixture.management_assignment WHERE membership_id=$1 AND status='ACTIVE'", [bMembership])).rows[0].n, 0);
  assert.equal((await b.query('SELECT count(*)::int AS n FROM tenant_fixture.organization_command_receipt WHERE command_id=$1', [commandId])).rows[0].n, 1);
  await denied('detached company command fence', () => gql(orgBase, ids.a, tokenA, detailQuery, { o: org, m: bMembership, r: ids.leave, v: 2 }, true));
  console.log('PASS suspend, source fence, resume, retained team, terminal detach and retained receipt');

  const principal = (await ops.query('SELECT principal_id FROM kabipay_ops.principal_login_link WHERE source_tenant_id=$1 AND source_user_id=$2', [ids.a, ids.aUser])).rows[0].principal_id;
  await ops.query("UPDATE kabipay_ops.principal SET status='DISABLED',identity_version=identity_version+1 WHERE id=$1", [principal]);
  await denied('central principal revocation', () => gql(orgBase, ids.a, tokenA, detailQuery, { o: org, m: bMembership, r: ids.leave, v: 2 }, true));
  console.log('PASS live denial checks');
}

async function cleanup() {
  if(reportingAcceptance){try{await reportingAcceptance.close();}catch(error){console.error(`reporting cleanup: ${error.message}`);process.exitCode=1;}}
  if(leaveProxy){leaveProxy.closeAllConnections();await new Promise(resolve=>leaveProxy.close(resolve));}
  for (const { child } of children) if (child.exitCode === null) child.kill();
  await Promise.allSettled(clients.map(c => c.end()));
  if (created) { try { docker(['rm', '-f', container]); } catch (e) { console.error(`cleanup: ${e.message}`); } }
  const resolved = path.resolve(temp), allowed = path.resolve(root, '.superpowers');
  if (resolved.startsWith(allowed + path.sep) && path.basename(resolved).startsWith('organization-execution-')) fs.rmSync(resolved, { recursive: true, force: true });
}
main().then(() => console.log('PASS real synthetic organization HTTP chain')).catch(e => { console.error(e.stack || e.message);for(const child of children.filter(item=>item.label==='organization'))for(const line of child.readLog().split(/\r?\n/))if(line.includes('ORGANIZATION_REPORTING_'))console.error(line); process.exitCode = 1; }).finally(cleanup);
