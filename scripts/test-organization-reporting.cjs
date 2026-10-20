// Actual central HTTP + ClickHouse + encrypted archive acceptance. Capture metadata is
// deliberately seeded here; real Debezium/Kafka capture proof lives in gap-lifecycle-acceptance.mjs.
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const {randomUUID}=require('node:crypto');
const {createServer}=require('node:http');
const {Pool}=require('../../hrms-database/node_modules/pg');

async function prepareOrganizationReportingAcceptance({opsUrl,temp,serviceKey,tenants}){
  assert(/^postgres:\/\/[^@]+@127\.0\.0\.1:\d+\/org_execution_fixture\?/.test(opsUrl),'owned disposable metadata database only');
  assert(tenants.length===2&&tenants.every(t=>/^[a-f0-9-]{36}$/.test(t)));
  process.env.ALLOW_ISOLATED_PLAINTEXT='true';
  const [{ClickHouseHistory},{EncryptedArchive},{archiveHandler},{ServiceProofVerifier},{parseEvent}]=await Promise.all([
    import('../integrations/organization-events/src/history.mjs'),import('../integrations/organization-events/src/archive.mjs'),import('../integrations/organization-events/src/archive-http.mjs'),import('../integrations/organization-events/src/service-proof.mjs'),import('../integrations/organization-events/src/events.mjs')]);
  const history=new ClickHouseHistory({url:'http://127.0.0.1:18123',user:'acceptance',password:'synthetic-only-password'});
  await history.query('SELECT 1 FORMAT JSONEachRow');
  const reporting=new ClickHouseHistory({url:'http://127.0.0.1:18123',user:'acceptance',password:'synthetic-only-password',table:'reporting_history'});
  const pool=new Pool({connectionString:opsUrl,max:4});
  const bindings={};const archive=new EncryptedArchive({root:path.join(temp,'history-archives'),keyVersion:'synthetic-v1',key:Buffer.alloc(32,91)});
  const proof=new ServiceProofVerifier([{kid:serviceKey.kid,service:'kabipay-organization',tenant_ids:tenants,public_pem:fs.readFileSync(serviceKey.publicFile,'utf8')}]);
  const server=createServer(archiveHandler({proof,pool,history,archive,bindings,audit:async(e,client)=>client.query('INSERT INTO kabipay_ops.organization_archive_access_audit(id,archive_id,organization_id,source_tenant_id,operation,actor_principal_id,actor_source_tenant_id,actor_source_user_id,row_count) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)',[e.id,e.archive_id,e.organization_id,e.source_tenant_id,e.operation,e.actor.principal_id,e.actor.source_tenant_id,e.actor.source_user_id,e.row_count])}));
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const passwordFile=path.join(temp,'clickhouse.synthetic.secret');fs.writeFileSync(passwordFile,'synthetic-only-password',{mode:0o600});
  let fixtureOrg;
  return {
    environment:{KABIPAY_ORGANIZATION_CLICKHOUSE_URL:'http://127.0.0.1:18123/',KABIPAY_ORGANIZATION_CLICKHOUSE_USER:'acceptance',KABIPAY_ORGANIZATION_CLICKHOUSE_PASSWORD_FILE:passwordFile,KABIPAY_ORGANIZATION_ARCHIVE_URL:`http://127.0.0.1:${server.address().port}/`},
    async run({ops,gql,centralUrl,sourceCompanyId,targetCompanyId,token,organizationId,membershipId,employeeId}){
      const org=organizationId,tenant=targetCompanyId,member=membershipId;fixtureOrg=org;bindings[tenant]=[org];
      const principal=(await ops.query("SELECT principal_id FROM kabipay_ops.principal_login_link WHERE source_tenant_id=$1 AND status='VERIFIED'",[sourceCompanyId])).rows[0].principal_id;
      const role=randomUUID(),binding=randomUUID();
      // Explicit synthetic current grants; production role management/installation is independently HTTP-tested.
      await ops.query("INSERT INTO kabipay_ops.organization_role(id,organization_id,name,status,version,created_at,updated_at) VALUES($1,$2,'Synthetic reporting rights','ACTIVE',1,now(),now())",[role,org]);
      for(const cap of ['analytics:read','history:read','history:export'])await ops.query("INSERT INTO kabipay_ops.organization_role_capability(role_id,organization_id,capability_code,employee_scope) VALUES($1,$2,$3,'ALL')",[role,org,cap]);
      await ops.query("INSERT INTO kabipay_ops.principal_role_binding(id,organization_id,principal_id,role_id,status,version,effective_from,created_at,updated_at) VALUES($1,$2,$3,$4,'ACTIVE',1,now()-interval '1 minute',now(),now())",[binding,org,principal,role]);
      await ops.query("INSERT INTO kabipay_ops.role_binding_company(binding_id,organization_id,organization_company_id,activation_status,expected_grant_version) VALUES($1,$2,$3,'ACTIVE',1)",[binding,org,member]);
      const membershipVersion=Number((await ops.query('SELECT version FROM kabipay_ops.organization_company WHERE id=$1',[member])).rows[0].version);
      const topic='organization.synthetic.central.'+randomUUID();const marker=randomUUID();
      await ops.query("INSERT INTO kabipay_ops.organization_company_capture(organization_id,source_tenant_id,membership_id,membership_version,acceptance_status,bootstrap_complete,topic,expected_partitions,history_from,source_bootstrap_through,accepted_at,reconciliation_sha256,accepted_by_principal_id) VALUES($1,$2,$3,$4,'ACCEPTED',true,$5,ARRAY[0,1],now()-interval '2 days',now(),now(),repeat('a',64),$6)",[org,tenant,member,membershipVersion,topic,principal]);
      await ops.query("INSERT INTO kabipay_ops.organization_capture_marker(marker_id,organization_id,source_tenant_id,membership_id,membership_version,topic,source_marker_at,source_lsn,cdc_lsn,bootstrap_complete,expected_partitions) VALUES($1,$2,$3,$4,$5,$6,now(),'0/100','0/200',true,ARRAY[0,1])",[marker,org,tenant,member,membershipVersion,topic]);
      for(const role of ['inbox','history','reporting'])for(const partition of [0,1]){
        await ops.query("INSERT INTO kabipay_ops.organization_consumer_checkpoint(consumer_role,organization_id,source_tenant_id,topic,partition_id,kafka_offset,state) VALUES($1,$2,$3,$4,$5,10,'READY')",[role,org,tenant,topic,partition]);
        await ops.query('INSERT INTO kabipay_ops.organization_capture_barrier(marker_id,consumer_role,partition_id,kafka_offset) VALUES($1,$2,$3,10)',[marker,role,partition]);
      }
      const occurred=new Date(Date.now()-300000).toISOString(),day=occurred.slice(0,10),entry=randomUUID(),leave=randomUUID();
      const events=[
        ['timesheet.changed','timesheet_entry',entry,{employee_id:employeeId,work_date:day,logged_seconds:3600,approved_seconds:1800}],
        ['project_time.changed','timesheet_entry',entry,{employee_id:employeeId,work_date:day,project_code:'SOURCE-PROJECT',logged_seconds:3600,approved_seconds:1800}],
        ['attendance.changed','attendance',randomUUID(),{employee_id:employeeId,work_date:day,status:'PRESENT',observed_seconds:null,overtime_seconds:0,late_minutes:2,early_exit_minutes:0}],
        ['leave.changed','leave_request',leave,{employee_id:employeeId,from_date:day,to_date:day,requested_centidays:50,status:'APPROVED'}],
        ['administration.changed','employee',employeeId,{resource_type:'employee',resource_id:employeeId,action:'employee_login_provision',status:'RECORDED'}],
      ].map(([event_type,aggregate_type,aggregate_id,payload])=>({schema_version:1,event_id:randomUUID(),event_type,organization_id:org,source_tenant_id:tenant,aggregate_type,aggregate_id,aggregate_version:1,occurred_at:occurred,deleted:false,actor:{kind:'system'},payload}));
      for(const raw of events){const e=parseEvent(Buffer.from(JSON.stringify(raw)),topic,{[topic]:{tenant_id:tenant,organization_id:org}});await history.write(e);await reporting.write(e);}
      const call=(query,variables={},error=false)=>gql(centralUrl,sourceCompanyId,token,query,variables,error);
      // Exercise the exact Rust query texts with nonempty rows as a separate diagnostic.
      const querySource=fs.readFileSync(path.join(__dirname,'../crates/kabipay-organization/src/operational_reporting.rs'),'utf8');
      for(const name of ['ATTENDANCE','LEAVE','PROJECT']){
        const query=querySource.match(new RegExp('const '+name+'[^=]*=\\s*"([^"]+)"'))[1];
        const url=new URL('http://127.0.0.1:18123/');url.searchParams.set('query',query);
        for(const [key,value]of Object.entries({organization:org,tenants:`['${tenant}']`,from:day,to:day}))url.searchParams.set('param_'+key,value);
        const response=await fetch(url,{method:'POST',headers:{'x-clickhouse-user':'acceptance','x-clickhouse-key':'synthetic-only-password'}});
        assert.equal(response.status,200,`${name} actual query: ${await response.text()}`);
      }
      const progressQuery='query($o:UUID!,$m:[UUID!],$f:NaiveDate!,$t:NaiveDate!){organizationOperationalProgress(organizationId:$o,membershipIds:$m,from:$f,to:$t)}';
      const variables={o:org,m:[member],f:day,t:day};
      let result=(await call(progressQuery,variables)).organizationOperationalProgress;
      assert.equal(result.partial,false,JSON.stringify(result));assert.equal(result.companies.length,1);let company=result.companies[0];
      assert.equal(company.freshness.state,'READY');assert.equal(company.attendance[0].observed_seconds,null);assert.equal(company.leave[0].requested_centidays,'50');assert.equal(company.project_time[0].logged_seconds,'3600');
      const simple=(await call('query($o:UUID!,$m:[UUID!],$f:NaiveDate!,$t:NaiveDate!){organizationProgress(organizationId:$o,membershipIds:$m,from:$f,to:$t)}',variables)).organizationProgress;assert.equal(simple.companies[0].logged_seconds,'3600');
      const from=new Date(Date.now()-3600000).toISOString(),through=new Date(Date.now()-60000).toISOString();
      const historyResult=(await call('query($o:UUID!,$m:[UUID!],$f:DateTime!,$t:DateTime!){organizationHistory(organizationId:$o,membershipIds:$m,from:$f,to:$t,first:100){items}}',{o:org,m:[member],f:from,t:through})).organizationHistory;
      assert.equal(historyResult.items.length,events.length);assert(historyResult.items.some(e=>e.event_type==='administration.changed'));assert(!JSON.stringify(historyResult).includes('password'));
      const archiveId=randomUUID();const exportQuery='mutation($o:UUID!,$m:UUID!,$a:UUID!,$f:DateTime!,$t:DateTime!){exportOrganizationHistoryArchive(organizationId:$o,membershipId:$m,archiveId:$a,from:$f,through:$t)}';
      const exported=(await call(exportQuery,{o:org,m:member,a:archiveId,f:from,t:through})).exportOrganizationHistoryArchive;
      assert.equal(exported.manifest.row_count,events.length);assert.equal(exported.items.length,events.length);
      const retrieveQuery='query($o:UUID!,$m:UUID!,$a:UUID!){organizationHistoryArchive(organizationId:$o,membershipId:$m,archiveId:$a)}';
      const retrieved=(await call(retrieveQuery,{o:org,m:member,a:archiveId})).organizationHistoryArchive;assert.deepEqual(retrieved.manifest,exported.manifest);
      const denied=await call(retrieveQuery,{o:org,m:member,a:randomUUID()},true);assert(denied.status>=400||denied.body.errors?.length);
      await ops.query("DELETE FROM kabipay_ops.organization_capture_barrier WHERE marker_id=$1 AND consumer_role='reporting' AND partition_id=1",[marker]);
      result=(await call(progressQuery,variables)).organizationOperationalProgress;assert.equal(result.partial,true);assert.equal(result.companies[0].attendance,null);
      const incomplete=await call(exportQuery,{o:org,m:member,a:randomUUID(),f:from,t:through},true);assert(incomplete.status>=400||incomplete.body.errors?.length);
      await ops.query("UPDATE kabipay_ops.principal_role_binding SET status='REVOKED' WHERE id=$1",[binding]);
      const revoked=await call(retrieveQuery,{o:org,m:member,a:archiveId},true);assert(revoked.status>=400||revoked.body.errors?.length);
      console.log('PASS actual central HTTP operational/history/archive APIs: separate company scope, exact/null facts, encrypted export/retrieve, wrong archive, incomplete partition, revoked current grant');
    },
    async close(){await new Promise(resolve=>server.close(resolve));await pool.end();if(fixtureOrg)for(const table of ['business_history','reporting_history'])await history.query(`ALTER TABLE organization.${table} DELETE WHERE organization_id={org:UUID} SETTINGS mutations_sync=1`,{org:fixtureOrg});}
  };
}
module.exports={prepareOrganizationReportingAcceptance};
