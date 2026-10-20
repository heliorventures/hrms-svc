// Full HTTP acceptance driver for the isolated organization execution fixture.
// The fixture owner supplies actual central/source processes, isolated PostgreSQL
// clients and explicit restart/revoke callbacks; no production connection default.
const assert = require('node:assert/strict');
const { randomUUID } = require('node:crypto');

function loopback(value) {
  const url = new URL(value);
  assert.equal(url.hostname, '127.0.0.1', 'payroll acceptance is loopback only');
  assert.equal(url.protocol, 'http:');
  assert(!url.username && !url.password && !url.search && !url.hash);
}
async function runPayrollJobAcceptance(context) {
  const { centralUrl, sourceUrl, sourceCompanyId, token, organizationId,
    membershipId, sourceDb, cycleId, restartSource, revokeGrant,
    wrongMembershipId } = context;
  loopback(centralUrl); loopback(sourceUrl);
  assert.equal(typeof restartSource, 'function');
  assert.equal(typeof revokeGrant, 'function');
  assert.equal(typeof wrongMembershipId, 'string', 'wrong-company membership is required for isolation acceptance');
  const database = (await sourceDb.query('SELECT current_database() AS name')).rows[0].name;
  assert(database.endsWith('_fixture'), 'payroll HTTP checks require a disposable fixture database');
  async function graphql(query, variables, allowError = false) {
    const response = await fetch(`${centralUrl}/graphql`, {
      method: 'POST', headers: { 'content-type': 'application/json',
        authorization: `Bearer ${token}`, 'x-login-company-id': sourceCompanyId },
      body: JSON.stringify({ query, variables }), signal: AbortSignal.timeout(45000),
    });
    const body = await response.json();
    if (!allowError) {
      assert.equal(response.status, 200, JSON.stringify(body));
      assert(!body.errors?.length, JSON.stringify(body.errors));
    }
    return { response, body };
  }
  async function version(type, id) {
    const rows = await sourceDb.query('SELECT version FROM organization_resource_version WHERE tenant_id=$1 AND resource_type=$2 AND resource_id=$3', [context.targetCompanyId, type, id]);
    assert.equal(rows.rowCount, 1); return Number(rows.rows[0].version);
  }
  const executeQuery = 'mutation($o:UUID!,$m:UUID!,$c:OrganizationCommandInput!){executeOrganizationCommand(organizationId:$o,membershipId:$m,command:$c){commandId status resourceVersion result}}';
  async function submit(operation, payload) {
    const command = { commandId: randomUUID(), resourceId: cycleId,
      expectedVersion: await version('payroll_run', cycleId), operation, payload };
    const variables = { o: organizationId, m: membershipId, c: command };
    const { body } = await graphql(executeQuery, variables);
    const accepted = body.data.executeOrganizationCommand;
    assert.equal(accepted.status, 'ACCEPTED', 'durable acceptance must precede business completion');
    assert.equal(accepted.result.job_id, command.commandId);
    assert.equal(accepted.result.state, 'QUEUED');
    return { command, accepted, variables };
  }
  async function job(id) {
    const rows = await sourceDb.query('SELECT state,result,failure_code,version,request_body,authority_reference FROM organization_payroll_job WHERE tenant_id=$1 AND id=$2', [context.targetCompanyId, id]);
    assert.equal(rows.rowCount, 1); return rows.rows[0];
  }
  async function runOne() {
    const response = await fetch(`${sourceUrl}/fixture/run-one`, { method: 'POST', signal: AbortSignal.timeout(90000) });
    assert.equal(response.status, 200); assert.equal((await response.json()).worked, true);
  }
  const draft = await submit('PAYROLL_DRAFT', { expected_revision: null });
  const count = await sourceDb.query('SELECT count(*)::int AS n FROM payroll_draft_calculation WHERE tenant_id=$1 AND cycle_id=$2', [context.targetCompanyId, cycleId]);
  assert.equal(count.rows[0].n, 0, 'acceptance must not synchronously calculate payroll');
  const replay = (await graphql(executeQuery, draft.variables)).body.data.executeOrganizationCommand;
  assert.deepEqual(replay, draft.accepted);
  const duplicates = await sourceDb.query('SELECT count(*)::int AS n FROM organization_payroll_job WHERE tenant_id=$1 AND id=$2', [context.targetCompanyId, draft.command.commandId]);
  assert.equal(duplicates.rows[0].n, 1);
  const concurrent=await graphql(executeQuery,{...draft.variables,c:{...draft.command,commandId:randomUUID()}},true);
  assert(concurrent.response.status>=400||concurrent.body.errors?.length||concurrent.body.data?.executeOrganizationCommand?.status==='REJECTED','distinct command must not create a second active job for the cycle');
  assert.equal((await sourceDb.query("SELECT count(*)::int AS n FROM organization_payroll_job WHERE tenant_id=$1 AND cycle_id=$2 AND state IN ('QUEUED','RUNNING')",[context.targetCompanyId,cycleId])).rows[0].n,1);
  const stored = await job(draft.command.commandId);
  assert(!stored.request_body.toString().includes('delegation_token'));
  assert(!JSON.stringify(stored.authority_reference).includes('delegation_token'));
  console.log('PASS payroll HTTP acceptance is asynchronous and exact replay creates one durable job');

  await restartSource();
  await runOne();
  const completed = await job(draft.command.commandId);
  assert.equal(completed.state, 'SUCCEEDED', JSON.stringify(completed));
  await restartSource();
  const noMore = await fetch(`${sourceUrl}/fixture/run-one`, { method: 'POST' });
  assert.equal((await noMore.json()).worked, false, 'terminal payroll work must not rerun');
  console.log('PASS payroll HTTP queued job survives process restart and completes once');

  const detailQuery = 'query($o:UUID!,$m:UUID!,$r:UUID!,$v:Int!){organizationResource(organizationId:$o,membershipId:$m,operation:PAYROLL_JOB_DETAIL,resourceId:$r,expectedVersion:$v){status result}}';
  const detailVars = { o: organizationId, m: membershipId, r: draft.command.commandId, v: Number(completed.version) };
  const read = await graphql(detailQuery, detailVars);
  assert.equal(read.body.data.organizationResource.result.state, 'SUCCEEDED');
  const memberVersion = (await sourceDb.query('SELECT version FROM organization_membership_installation WHERE tenant_id=$1 AND membership_id=$2', [context.targetCompanyId, membershipId])).rows[0].version;
  const payrollReview=await graphql('query($o:UUID!,$m:UUID!,$r:UUID!,$v:Int!,$p:JSON!){organizationResource(organizationId:$o,membershipId:$m,operation:PAYROLL_DETAIL,resourceId:$r,expectedVersion:$v,payload:$p){result}}',{o:organizationId,m:membershipId,r:cycleId,v:await version('payroll_run',cycleId),p:{cycle_id:cycleId}});
  assert.equal(payrollReview.body.data.organizationResource.result.cycle.id,cycleId);
  assert.equal(payrollReview.body.data.organizationResource.result.draft.revision,completed.result.revision);
  assert.equal(payrollReview.body.data.organizationResource.result.draft.fingerprint,completed.result.fingerprint);
  const wrongList=await graphql('query($o:UUID!,$m:UUID!,$v:Int!,$p:JSON!){organizationResource(organizationId:$o,membershipId:$m,operation:PAYROLL_LIST,resourceId:$m,expectedVersion:$v,payload:$p){status result}}',{o:organizationId,m:membershipId,v:Number(memberVersion),p:{cycle_id:cycleId}},true);
  assert(wrongList.response.status>=400||wrongList.body.errors?.length||wrongList.body.data?.organizationResource?.status==='REJECTED','collection list cannot alias source cycle detail');
  console.log('PASS payroll HTTP source cycle detail returns the actual review and list cannot alias detail');
  const listed = await graphql('query($o:UUID!,$m:UUID!,$v:Int!){organizationResource(organizationId:$o,membershipId:$m,operation:PAYROLL_JOB_LIST,resourceId:$m,expectedVersion:$v){result}}', { o: organizationId, m: membershipId, v: Number(memberVersion) });
  assert(listed.body.data.organizationResource.result.jobs.some(item => item.job_id === draft.command.commandId));
  if (wrongMembershipId) {
    const denied = await graphql(detailQuery, { ...detailVars, m: wrongMembershipId }, true);
    assert(denied.response.status >= 400 || denied.body.errors?.length || denied.body.data?.organizationResource?.status === 'REJECTED');
    console.log('PASS payroll HTTP wrong-company job detail is denied');
  }

  const cancellable = await submit('PAYROLL_DRAFT', { expected_revision: completed.result.revision });
  const cancellation = await graphql(executeQuery, { o: organizationId, m: membershipId,
    c: { commandId: randomUUID(), resourceId: cancellable.command.commandId, expectedVersion: 1,
      operation: 'PAYROLL_JOB_CANCEL_BEFORE_EXECUTION', payload: {} } });
  assert.equal(cancellation.body.data.executeOrganizationCommand.result.state, 'CANCELLED');
  const cancelledRun = await fetch(`${sourceUrl}/fixture/run-one`, { method: 'POST' });
  assert.equal((await cancelledRun.json()).worked, false);
  console.log('PASS payroll HTTP protected status/list and cancellation before execution');

  const finalize = await submit('PAYROLL_FINALIZE', { revision: completed.result.revision,
    fingerprint: '0'.repeat(64), acknowledgement: { provisional_tax_employees: [] } });
  await runOne();
  const stale = await job(finalize.command.commandId);
  assert.equal(stale.state, 'FAILED');
  assert.equal(stale.failure_code, 'BUSINESS_VALIDATION_FAILED');
  const payslips = await sourceDb.query('SELECT count(*)::int AS n FROM payslip WHERE tenant_id=$1 AND payroll_cycle_id=$2', [context.targetCompanyId, cycleId]);
  assert.equal(payslips.rows[0].n, 0);
  console.log('PASS payroll HTTP queued finalization preserves stale fingerprint rejection');

  assert.equal(completed.result.can_finalize,true,'fixture must contain genuine reviewed salary input that can finalize');
  const validFinalization=await submit('PAYROLL_FINALIZE',{revision:completed.result.revision,fingerprint:completed.result.fingerprint,acknowledgement:{provisional_tax_employees:[]}});
  await runOne();
  const finalized=await job(validFinalization.command.commandId);
  assert.equal(finalized.state,'SUCCEEDED',JSON.stringify(finalized));
  assert.equal(finalized.result.status,'PROCESSED');
  const paid=await sourceDb.query('SELECT gross_salary,net_salary FROM payslip WHERE tenant_id=$1 AND payroll_cycle_id=$2',[context.targetCompanyId,cycleId]);
  assert.equal(paid.rowCount,1);assert.equal(Number(paid.rows[0].gross_salary),1000);assert.equal(Number(paid.rows[0].net_salary),1000);
  const attribution=(await sourceDb.query('SELECT processed_by,processed_by_principal_id FROM payroll_cycle WHERE tenant_id=$1 AND id=$2',[context.targetCompanyId,cycleId])).rows[0];
  assert.equal(attribution.processed_by,null,'foreign worker must not impersonate a linked target-company user');
  assert.equal(attribution.processed_by_principal_id,completed.authority_reference.principal_id);
  const receipt=(await sourceDb.query('SELECT outcome,principal_id,source_tenant_id,source_user_id FROM organization_command_receipt WHERE tenant_id=$1 AND command_id=$2',[context.targetCompanyId,validFinalization.command.commandId])).rows[0];
  assert.equal(receipt.outcome.status,'ACCEPTED','business completion must not rewrite immutable delivery acceptance');
  assert.equal(receipt.principal_id,completed.authority_reference.principal_id);assert.equal(receipt.source_tenant_id,completed.authority_reference.source_tenant_id);assert.equal(receipt.source_user_id,completed.authority_reference.source_user_id);
  await restartSource();
  assert.equal((await (await fetch(`${sourceUrl}/fixture/run-one`,{method:'POST'})).json()).worked,false);
  assert.equal((await sourceDb.query('SELECT count(*)::int AS n FROM payslip WHERE tenant_id=$1 AND payroll_cycle_id=$2',[context.targetCompanyId,cycleId])).rows[0].n,1);
  console.log('PASS payroll HTTP valid reviewed finalization creates one actual payslip and survives restart without replay');

  const revoked = await submit('PAYROLL_DRAFT', { expected_revision: completed.result.revision });
  await revokeGrant();
  await restartSource();
  await runOne();
  const refused = await job(revoked.command.commandId);
  assert.equal(refused.state, 'FAILED');
  assert.equal(refused.failure_code, 'AUTHORITY_UNAVAILABLE');
  const revision = await sourceDb.query('SELECT revision FROM payroll_draft_calculation WHERE tenant_id=$1 AND cycle_id=$2', [context.targetCompanyId, cycleId]);
  assert.equal(revision.rows[0].revision, completed.result.revision);
  const statusEvents=(await sourceDb.query("SELECT event_envelope FROM company_event_outbox WHERE aggregate_type='payroll_job' AND aggregate_id=$1 ORDER BY aggregate_version",[revoked.command.commandId])).rows.map(row=>row.event_envelope);
  assert.deepEqual(statusEvents.map(event=>event.payload.status),['QUEUED','RUNNING','FAILED']);
  for(const event of statusEvents){
    assert.equal(event.payload.action,'payroll_job_status');
    assert.deepEqual(Object.keys(event.payload).sort(),['action','resource_id','resource_type','status']);
    assert.equal(event.actor.principal_id,completed.authority_reference.principal_id);
    assert.equal(event.actor.command_id,revoked.command.commandId);
  }
  console.log('PASS payroll HTTP revoked grant is rechecked after restart before business commit');
  console.log('PASS payroll source status history retains redacted immutable actor transitions after grant revocation');
  return { draftJobId: draft.command.commandId, staleJobId: finalize.command.commandId,
    revokedJobId: revoked.command.commandId };
}

module.exports = { runPayrollJobAcceptance };
if (require.main === module) {
  throw Error('Run this driver from the disposable organization execution fixture with explicit processes and database clients.');
}
