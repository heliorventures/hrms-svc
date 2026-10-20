// Real HTTP driver for the disposable separate-company fixture. The fixture
// owner supplies running services, verified login links and explicit grants.
const assert = require('node:assert/strict');
const {randomUUID} = require('node:crypto');
const {Client}=require('../../hrms-database/node_modules/pg');

async function runHrAdministrationAcceptance(ctx) {
  const {centralUrl,sourceCompanyId,targetCompanyId,token,organizationId,membershipId,
    sourceDb,opsDb,employeeId,adminRoleId,staffRoleId,selfAdminUserId,revokeAdministrationGrant,beforeAdministrationRevocation}=ctx;
  const endpoint=new URL(centralUrl);
  assert.equal(endpoint.hostname,'127.0.0.1');assert.equal(endpoint.protocol,'http:');
  assert(!endpoint.username&&!endpoint.password&&!endpoint.search&&!endpoint.hash);
  assert.notEqual(sourceCompanyId,targetCompanyId,'HR acceptance requires distinct origin and target companies');
  assert((await sourceDb.query('SELECT current_database() AS name')).rows[0].name.endsWith('_fixture'));
  for(const id of [employeeId,adminRoleId,staffRoleId,selfAdminUserId])assert.equal(typeof id,'string');
  assert.equal(typeof revokeAdministrationGrant,'function');
  assert(opsDb,'credential persistence checks require the owned metadata database');
  const mutation='mutation($o:UUID!,$m:UUID!,$c:OrganizationCommandInput!){executeOrganizationCommand(organizationId:$o,membershipId:$m,command:$c){commandId status resourceVersion result}}';
  const readOperations=new Set(['EMPLOYEE_DETAIL','WORKFLOW_DETAIL','SEPARATION_DETAIL']);
  const readQuery='query($o:UUID!,$m:UUID!,$op:OrganizationOperation!,$r:UUID!,$v:Int!,$p:JSON!){organizationResource(organizationId:$o,membershipId:$m,operation:$op,resourceId:$r,expectedVersion:$v,payload:$p){status resourceVersion result}}';
  async function send(command,denied=false){
    const reading=readOperations.has(command.operation);
    const variables=reading?{o:organizationId,m:membershipId,op:command.operation,r:command.resourceId,v:command.expectedVersion,p:command.payload}:{o:organizationId,m:membershipId,c:command};
    const response=await fetch(`${centralUrl}/graphql`,{method:'POST',headers:{'content-type':'application/json',authorization:`Bearer ${token}`,'x-login-company-id':sourceCompanyId},body:JSON.stringify({query:reading?readQuery:mutation,variables}),signal:AbortSignal.timeout(45000)});
    const body=await response.json();
    const result=body.data?.[reading?'organizationResource':'executeOrganizationCommand'];
    if(denied){assert(body.errors?.length||response.status>=400||result?.status==='REJECTED',`Expected administration request to fail: ${JSON.stringify(body)}`);return body;}
    assert.equal(response.status,200);assert(!body.errors?.length,JSON.stringify(body.errors));
    assert(result);assert.equal(result.status,'COMMITTED',JSON.stringify(result));return result;
  }
  async function version(type,id){const row=await sourceDb.query('SELECT version FROM organization_resource_version WHERE tenant_id=$1 AND resource_type=$2 AND resource_id=$3',[targetCompanyId,type,id]);assert.equal(row.rowCount,1);return Number(row.rows[0].version);}
  async function command(operation,type,id,payload,expectedVersion){return{commandId:randomUUID(),resourceId:id,expectedVersion:expectedVersion??await version(type,id),operation,payload};}
  async function run(operation,type,id,payload,expectedVersion){return send(await command(operation,type,id,payload,expectedVersion));}
  async function reject(operation,type,id,payload,expectedVersion){return send(await command(operation,type,id,payload,expectedVersion),true);}
  async function noCredentialRetention(commandId,password){
    const intent=(await opsDb.query('SELECT to_jsonb(i) AS body FROM kabipay_ops.organization_command_intent i WHERE command_id=$1',[commandId])).rows[0].body;
    for(const key of ['command_body_base64','request_body','payload','initial_password'])assert(!Object.hasOwn(intent,key));
    assert(!JSON.stringify(intent).includes(password));
    const outbox=(await opsDb.query('SELECT to_jsonb(o) AS body FROM kabipay_ops.organization_outbox o WHERE organization_id=$1',[organizationId])).rows;
    assert(!JSON.stringify(outbox).includes(password));assert(!JSON.stringify(outbox).includes('initial_password'));
    const receipt=(await sourceDb.query('SELECT outcome FROM organization_command_receipt WHERE tenant_id=$1 AND command_id=$2',[targetCompanyId,commandId])).rows[0].outcome;
    assert(!JSON.stringify(receipt).includes(password));assert(!JSON.stringify(receipt).includes('password_hash'));
  }

  const detail=await run('EMPLOYEE_DETAIL','employee',employeeId,{});
  assert.equal(detail.result.employee_id,employeeId);
  const createdEmployee=randomUUID();
  const create=await command('EMPLOYEE_CREATE','employee',createdEmployee,{employee_code:`fixture-${createdEmployee.slice(0,8)}`,first_name:'Shared',last_name:'Administration',date_of_joining:'2026-10-01'},1);
  const saved=await send(create);assert.equal(saved.result.employee_id,createdEmployee);
  assert.deepEqual(await send(create),saved,'Exact employee creation replay must return the same receipt');
  assert.equal(Number((await sourceDb.query('SELECT count(*) AS n FROM employee WHERE tenant_id=$1 AND id=$2',[targetCompanyId,createdEmployee])).rows[0].n),1);
  const parameters=sourceDb.connectionParameters;
  const holder=new Client({host:parameters.host,port:parameters.port,database:parameters.database,user:parameters.user,password:parameters.password});
  await holder.connect();
  let pendingUpdate;
  try{
    await holder.query('BEGIN');
    await holder.query("SELECT set_config('search_path',$1,true)",[(await sourceDb.query("SELECT current_setting('search_path') AS path")).rows[0].path]);
    await holder.query('SELECT pg_advisory_xact_lock(hashtextextended($1,0))',[`company-account-admin:${targetCompanyId}`]);
    const holderPid=(await holder.query('SELECT pg_backend_pid() AS pid')).rows[0].pid;
    pendingUpdate=run('EMPLOYEE_UPDATE','employee',createdEmployee,{first_name:'Concurrent'}).then(value=>({value}),error=>({error}));
    let blocked=false;
    for(let attempt=0;attempt<150;attempt++){
      blocked=(await sourceDb.query("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event='advisory' AND $1=ANY(pg_blocking_pids(pid))) AS blocked",[holderPid])).rows[0].blocked;
      if(blocked)break;
      await new Promise(resolve=>setTimeout(resolve,100));
    }
    assert(blocked,'source update must wait on company account advisory lock before employee lock');
    await holder.query('SELECT id FROM employee WHERE tenant_id=$1 AND id=$2 FOR UPDATE NOWAIT',[targetCompanyId,createdEmployee]);
    await holder.query('COMMIT');
    const result=await pendingUpdate;if(result.error)throw result.error;
  }finally{await holder.query('ROLLBACK');await holder.end();if(pendingUpdate)await pendingUpdate;}
  console.log('PASS HR HTTP employee update obtains the account advisory lock before the employee row');
  const before=await version('employee',createdEmployee);
  await run('EMPLOYEE_UPDATE','employee',createdEmployee,{first_name:'Changed'});
  await reject('EMPLOYEE_UPDATE','employee',createdEmployee,{first_name:'Stale'},before);
  await reject('EMPLOYEE_UPDATE','employee',createdEmployee,{user_id:selfAdminUserId});
  await reject('EMPLOYEE_EMPLOYMENT','employee',createdEmployee,{status:'INVENTED'});
  await run('EMPLOYEE_EMPLOYMENT','employee',createdEmployee,{status:'ON_LEAVE'});
  console.log('PASS HR HTTP employee create/update/lifecycle, exact replay, version and protected fields');

  const loginPassword=`fixture-${randomUUID()}`;
  const provision=await command('EMPLOYEE_LOGIN_PROVISION','employee',createdEmployee,{username:`login-${createdEmployee}`,email:`${createdEmployee}@fixture.invalid`,initial_password:loginPassword,role_ids:[staffRoleId]});
  const login=await send(provision);assert.equal(login.result.employee_id,createdEmployee);assert.equal(login.result.has_login,true);
  const loginUser=login.result.user_id;assert.equal(typeof loginUser,'string');
  const provisioned=(await sourceDb.query('SELECT e.user_id,u.must_change_password,u.password_hash FROM employee e JOIN "user" u ON u.tenant_id=e.tenant_id AND u.id=e.user_id WHERE e.tenant_id=$1 AND e.id=$2',[targetCompanyId,createdEmployee])).rows[0];
  assert.equal(provisioned.user_id,loginUser);assert.equal(provisioned.must_change_password,true);assert(provisioned.password_hash.length>20);assert.notEqual(provisioned.password_hash,loginPassword);
  assert.equal((await sourceDb.query('SELECT count(*)::int AS n FROM user_role WHERE user_id=$1 AND role_id=$2',[loginUser,staffRoleId])).rows[0].n,1);
  assert.deepEqual(await send(provision),login);
  await reject('EMPLOYEE_LOGIN_PROVISION','employee',createdEmployee,provision.payload);
  assert.equal((await sourceDb.query('SELECT count(*)::int AS n FROM "user" WHERE tenant_id=$1 AND username=$2',[targetCompanyId,provision.payload.username])).rows[0].n,1);
  assert.equal((await run('EMPLOYEE_DETAIL','employee',createdEmployee,{})).result.has_login,true);
  await noCredentialRetention(provision.commandId,loginPassword);
  console.log('PASS HR HTTP actual employee login provisioning, exact replay, repeated provisioning denial and credential non-retention');

  // Account creation uses the production hash/provisioning path. A fixture-only
  // password-change completion makes its new administrator eligible for safety cases.
  const otherAdmin=randomUUID();
  const password=`fixture-${randomUUID()}`;
  const accountCommand=await command('ACCOUNT_CREATE','user',otherAdmin,{username:`fixture-${otherAdmin}`,email:`${otherAdmin}@fixture.invalid`,initial_password:password,role_ids:[adminRoleId]},1);
  const account=await send(accountCommand);
  await noCredentialRetention(accountCommand.commandId,password);
  assert.equal(account.result.user_id,otherAdmin);
  assert(!JSON.stringify(account).includes(password));assert(!JSON.stringify(account).includes('password_hash'));
  assert.equal((await sourceDb.query('SELECT must_change_password FROM "user" WHERE tenant_id=$1 AND id=$2',[targetCompanyId,otherAdmin])).rows[0].must_change_password,true);
  await sourceDb.query('UPDATE "user" SET must_change_password=false WHERE tenant_id=$1 AND id=$2',[targetCompanyId,otherAdmin]);
  await run('ACCOUNT_UPDATE','user',otherAdmin,{username:`EDITED-${otherAdmin}`});
  assert.equal((await sourceDb.query('SELECT username FROM "user" WHERE tenant_id=$1 AND id=$2',[targetCompanyId,otherAdmin])).rows[0].username,`edited-${otherAdmin}`);
  await reject('ACCOUNT_ROLES','user',otherAdmin,{role_ids:[]});
  const wrongRole=randomUUID();
  await sourceDb.query('INSERT INTO role(id,tenant_id,name,is_system_role,is_deleted,created_at,updated_at) VALUES($1,$2,$3,false,false,now(),now())',[wrongRole,randomUUID(),`Wrong fixture ${wrongRole}`]);
  await reject('ACCOUNT_ROLES','user',otherAdmin,{role_ids:[wrongRole]});
  await run('ACCOUNT_ROLES','user',otherAdmin,{role_ids:[staffRoleId]});
  await run('ACCOUNT_ROLES','user',otherAdmin,{role_ids:[adminRoleId]});
  // The fixture must provide an actual verified target-company login link for
  // selfAdminUserId. The other active administrator prevents a last-admin denial
  // from disguising a missing logical-self restriction.
  await reject('ACCOUNT_ACCESS','user',selfAdminUserId,{is_active:false});
  await reject('ACCOUNT_ROLES','user',selfAdminUserId,{role_ids:[staffRoleId]});
  await sourceDb.query('UPDATE "user" SET is_active=false WHERE tenant_id=$1 AND id=$2',[targetCompanyId,selfAdminUserId]);
  try{
    await reject('ACCOUNT_ACCESS','user',otherAdmin,{is_active:false});
    await reject('ACCOUNT_ROLES','user',otherAdmin,{role_ids:[staffRoleId]});
  }finally{await sourceDb.query('UPDATE "user" SET is_active=true WHERE tenant_id=$1 AND id=$2',[targetCompanyId,selfAdminUserId]);}
  await run('ACCOUNT_ACCESS','user',otherAdmin,{is_active:false});
  await run('ACCOUNT_ACCESS','user',otherAdmin,{is_active:true});
  console.log('PASS HR HTTP account create/update/access/roles, logical self and last administrator protections');

  const workflow=randomUUID(),secondWorkflow=randomUUID();
  const definition={name:'Fixture travel approval',entity_type:'TRAVEL_REQUEST',is_active:false,steps:[{step_name:'Manager review',approver_type:'REPORTING_MANAGER',can_skip:false,sla_hours:24}]};
  await reject('WORKFLOW_SAVE','workflow_definition',workflow,{...definition,entity_type:'PAYROLL'},1);
  await run('WORKFLOW_SAVE','workflow_definition',workflow,definition,1);
  await run('WORKFLOW_ACTIVATION','workflow_definition',workflow,{is_active:true});
  await run('WORKFLOW_SAVE','workflow_definition',secondWorkflow,{...definition,name:'Second travel approval'},1);
  await reject('WORKFLOW_ACTIVATION','workflow_definition',secondWorkflow,{is_active:true});
  const workflowDetail=await run('WORKFLOW_DETAIL','workflow_definition',workflow,{});
  assert.equal(workflowDetail.result.steps[0].approver_permission,'travel:approve');
  const historical=(await sourceDb.query('SELECT w.id,w.name,w.entity_type FROM workflow w WHERE w.tenant_id=$1 AND EXISTS(SELECT 1 FROM workflow_instance i WHERE i.tenant_id=w.tenant_id AND i.workflow_id=w.id) LIMIT 1',[targetCompanyId])).rows[0];
  assert(historical,'Fixture must include a workflow with real source execution history');
  await reject('WORKFLOW_SAVE','workflow_definition',historical.id,{...definition,name:historical.name,entity_type:historical.entity_type});
  console.log('PASS HR HTTP workflow save/detail/activation and execution history preservation');

  const separation=randomUUID();
  const exit={employee_id:createdEmployee,separation_type:'RESIGNATION',resignation_date:'2026-10-19',last_working_date:'2026-10-31',reason:'Synthetic fixture'};
  await reject('SEPARATION_CREATE','separation',separation,{...exit,approved_by:selfAdminUserId},1);
  await run('SEPARATION_CREATE','separation',separation,exit,1);
  await run('SEPARATION_CANCEL_APPROVAL','separation',separation,{});
  const clearance=randomUUID();
  await sourceDb.query('INSERT INTO clearance_checklist(id,tenant_id,separation_id,department,task_name,is_cleared,created_at,updated_at) VALUES($1,$2,$3,$4,$5,false,now(),now())',[clearance,targetCompanyId,separation,'IT','Synthetic operational clearance']);
  const cleared=await run('CLEARANCE_UPDATE','clearance_checklist',clearance,{is_cleared:true});
  assert(cleared.result.cleared_by_principal_id);
  const attribution=(await sourceDb.query('SELECT cleared_by,cleared_by_principal_id FROM clearance_checklist WHERE tenant_id=$1 AND id=$2',[targetCompanyId,clearance])).rows[0];
  assert.equal(attribution.cleared_by,null);assert(attribution.cleared_by_principal_id);
  const rejection=await command('SEPARATION_REJECT','separation',separation,{});
  const rejected=await send(rejection);assert.equal(rejected.result.status,'REJECTED');
  assert.deepEqual(await send(rejection),rejected);
  await reject('SEPARATION_CANCEL_APPROVAL','separation',separation,{});
  const rejectedActor=(await sourceDb.query('SELECT approved_by,approved_by_principal_id FROM separation WHERE tenant_id=$1 AND id=$2',[targetCompanyId,separation])).rows[0];
  assert.equal(rejectedActor.approved_by,null);assert(rejectedActor.approved_by_principal_id);
  console.log('PASS HR HTTP separation initiation/rejection/cancellation and logical clearance attribution');

  if(beforeAdministrationRevocation)await beforeAdministrationRevocation();
  await revokeAdministrationGrant();
  await reject('EMPLOYEE_DETAIL','employee',createdEmployee,{});
  await reject('ACCOUNT_ACCESS','user',otherAdmin,{is_active:false});
  await reject('WORKFLOW_DETAIL','workflow_definition',workflow,{});
  await reject('SEPARATION_DETAIL','separation',separation,{});
  console.log('PASS HR HTTP revoked administration grants deny employee/account/workflow/exit access');
  return{employeeId:createdEmployee,accountId:otherAdmin,workflowId:workflow,separationId:separation};
}
module.exports={runHrAdministrationAcceptance};
