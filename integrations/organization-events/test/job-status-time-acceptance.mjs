import assert from 'node:assert/strict';import {readFileSync} from 'node:fs';import {randomUUID} from 'node:crypto';import pg from 'pg';
if(process.env.ORGANIZATION_SYNTHETIC_ACCEPTANCE!=='true')throw Error('EXPLICIT_SYNTHETIC_OPT_IN_REQUIRED');
const admin=new pg.Pool({connectionString:'postgres://acceptance:synthetic-only-password@127.0.0.1:15436/postgres'}),db='organization_jobtime_'+randomUUID().replaceAll('-','');let source;
const sql=name=>readFileSync(new URL('../../../../hrms-database/changelog/migrations/'+name,import.meta.url),'utf8').match(/<!\[CDATA\[([\s\S]*?)\]\]>/)[1].replaceAll('${schema}','public');
const tenant=randomUUID(),org=randomUUID(),member=randomUUID(),actor=randomUUID(),job=randomUUID(),cycle=randomUUID(),authority=randomUUID();
try{
 await admin.query(`CREATE DATABASE ${db}`);source=new pg.Pool({connectionString:'postgres://acceptance:synthetic-only-password@127.0.0.1:15436/'+db});
 await source.query('CREATE TABLE payroll_cycle(id uuid PRIMARY KEY);CREATE TABLE organization_resource_version(tenant_id uuid,resource_type text,resource_id uuid,version bigint,PRIMARY KEY(tenant_id,resource_type,resource_id));CREATE TABLE organization_membership_installation(tenant_id uuid,organization_id uuid,membership_id uuid,version bigint,status text);CREATE TABLE company_event_outbox(id uuid PRIMARY KEY,source_tenant_id uuid,organization_id uuid,event_type text,aggregate_type text,aggregate_id uuid,aggregate_version bigint,event_envelope jsonb)');
 await source.query(sql('0119_organization_payroll_jobs/organization_payroll_jobs.xml'));
 const history=sql('0121_organization_source_history/organization_source_history.xml');
 await source.query(history.slice(history.indexOf('CREATE FUNCTION "public".organization_fact_uuid('),history.indexOf('ALTER FUNCTION "public".organization_emit_fact(')));
 await source.query(history.slice(history.indexOf('CREATE FUNCTION "public".organization_payroll_status_history()'),history.indexOf('CREATE TABLE "public".organization_history_export (')));
 await source.query('INSERT INTO payroll_cycle VALUES($1)',[cycle]);await source.query("INSERT INTO organization_membership_installation VALUES($1,$2,$3,1,'ACTIVE')",[tenant,org,member]);
 const ref={tenant_id:tenant,organization_id:org,membership_id:member,principal_id:actor,source_tenant_id:tenant,source_user_id:actor,identity_version:1,membership_version:1,authority_kind:'organization_grant',authority_id:authority,authority_version:1,actor:{principal_id:actor,display_name:'Synthetic operator',identity_version:1}};
 await source.query("INSERT INTO organization_payroll_job(tenant_id,id,cycle_id,principal_id,organization_id,membership_id,authority_id,authority_version,operation,request_body,request_hash,authority_reference,expected_version) VALUES($1,$2,$3,$4,$5,$6,$7,1,'payroll_draft','',repeat('a',64),$8,1)",[tenant,job,cycle,actor,org,member,authority,ref]);
 await source.query("UPDATE organization_payroll_job SET state='RUNNING',started_at=clock_timestamp()-interval '1 day',lease_token=$1,lease_expires_at=clock_timestamp()+interval '1 minute' WHERE id=$2",[randomUUID(),job]);
 await source.query('SELECT pg_sleep(0.02)');
 await source.query("UPDATE organization_payroll_job SET state='QUEUED',lease_token=NULL,lease_expires_at=NULL,next_attempt_at=clock_timestamp()+interval '1 minute' WHERE id=$1",[job]);
 const events=(await source.query('SELECT event_envelope FROM company_event_outbox ORDER BY aggregate_version')).rows.map(r=>r.event_envelope);assert.deepEqual(events.map(e=>e.payload.status),['QUEUED','RUNNING','QUEUED']);
 assert(Date.parse(events[2].occurred_at)>Date.parse(events[1].occurred_at),'retry fact must have its own later source transition time');
 assert(Date.parse(events[1].occurred_at)>Date.now()-60000,'running fact time must not reuse historical started_at');
 for(const e of events){assert.equal(e.actor.principal_id,actor);assert.deepEqual(Object.keys(e.payload).sort(),['action','resource_id','resource_type','status']);}
 console.log(JSON.stringify({result:'PASS',retry_fact_time_advances:true,lifecycle_started_at_not_reused:true,original_actor_and_closed_payload_preserved:true}));
}finally{await source?.end();for(let i=0;i<40;i++){if((await admin.query('SELECT count(*)::int AS n FROM pg_stat_activity WHERE datname=$1',[db])).rows[0].n===0)break;await new Promise(r=>setTimeout(r,25));}await admin.query(`DROP DATABASE IF EXISTS ${db}`);await admin.end();}
