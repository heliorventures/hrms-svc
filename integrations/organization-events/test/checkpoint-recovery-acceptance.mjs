// Fresh synthetic metadata database only; never an existing deployment database.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {randomUUID} from 'node:crypto';
import pg from 'pg';
import {ProjectionStore} from '../src/store.mjs';
if(process.env.ORGANIZATION_SYNTHETIC_ACCEPTANCE!=='true')throw Error('EXPLICIT_SYNTHETIC_OPT_IN_REQUIRED');
const admin=new pg.Pool({connectionString:'postgres://acceptance:synthetic-only-password@127.0.0.1:15436/postgres'}),db='organization_resume_'+randomUUID().replaceAll('-','');
const org=randomUUID(),tenant=randomUUID(),member=randomUUID(),principal=randomUUID(),marker=randomUUID(),topic='synthetic.resume';let pool;
const migration=name=>readFileSync(new URL('../../../../hrms-database/changelog/migrations/'+name,import.meta.url),'utf8').match(/<!\[CDATA\[([\s\S]*?)\]\]>/)[1];
try{
 await admin.query(`CREATE DATABASE ${db}`);pool=new pg.Pool({connectionString:'postgres://acceptance:synthetic-only-password@127.0.0.1:15436/'+db});
 await pool.query("CREATE SCHEMA kabipay_ops;CREATE TABLE kabipay_ops.organization(id uuid PRIMARY KEY,status text DEFAULT 'ACTIVE');CREATE TABLE kabipay_ops.tenant(id uuid PRIMARY KEY,status text DEFAULT 'ACTIVE',is_deleted boolean DEFAULT false);CREATE TABLE kabipay_ops.principal(id uuid PRIMARY KEY);CREATE TABLE kabipay_ops.organization_company(id uuid PRIMARY KEY,organization_id uuid,tenant_id uuid,version bigint DEFAULT 2,installed_version bigint DEFAULT 2,runtime_status text DEFAULT 'ACTIVE',status text DEFAULT 'ACTIVE',effective_from timestamptz DEFAULT now(),effective_to timestamptz)");
 await pool.query('INSERT INTO kabipay_ops.organization(id) VALUES($1)',[org]);await pool.query('INSERT INTO kabipay_ops.tenant(id) VALUES($1)',[tenant]);await pool.query('INSERT INTO kabipay_ops.organization_company(id,organization_id,tenant_id) VALUES($1,$2,$3)',[member,org,tenant]);await pool.query('INSERT INTO kabipay_ops.principal VALUES($1)',[principal]);
 await pool.query(migration('0116_organization_projections/organization_projections.xml'));await pool.query(migration('0120_organization_capture_lifecycle/organization_capture_lifecycle.xml'));
 const store=new ProjectionStore(pool),e={organization_id:org,source_tenant_id:tenant,occurred_at:new Date().toISOString()};
 async function mark(role,state,code,offset='100'){await store.transaction(tx=>tx.checkpoint(e,{role,topic,partition:0,offset,state,code}));}
 async function row(role){return (await pool.query('SELECT *,acknowledged_at::text AS acknowledged_at FROM kabipay_ops.organization_consumer_checkpoint WHERE consumer_role=$1',[role])).rows[0];}
 await mark('inbox','PARTIAL','HISTORICAL_MEMBERSHIP');await mark('inbox','PARTIAL','INVALID_PAYLOAD');assert.equal((await row('inbox')).detail_code,'INVALID_PAYLOAD','real invalid must not be masked by legacy historical code');await mark('inbox','READY',null);assert.equal((await row('inbox')).state,'PARTIAL');
 await mark('history','QUARANTINED','VERSION_CONFLICT');await mark('history','PARTIAL','INVALID_PAYLOAD');await mark('history','READY',null);assert.equal((await row('history')).state,'QUARANTINED');assert.equal((await row('history')).detail_code,'VERSION_CONFLICT');
 await mark('reporting','PARTIAL','HISTORICAL_MEMBERSHIP');await mark('reporting','READY',null);assert.equal((await row('reporting')).state,'PARTIAL','legacy recovery is never automatic');
 const classification=async(id,epoch)=>(await pool.query('SELECT kabipay_ops.organization_capture_marker_classification($1,$2,$3,$4) AS state',[org,tenant,id,epoch])).rows[0].state;
 assert.equal(await classification(member,2),'ACTIVE');assert.equal(await classification(member,1),'HISTORICAL');assert.equal(await classification(randomUUID(),1),'NONE');assert.equal(await classification(member,3),'NONE');
 for(const status of ['SUSPENDED','DETACHED']){await pool.query('UPDATE kabipay_ops.organization_company SET status=$1',[status]);assert.equal(await classification(member,2),'HISTORICAL');}await pool.query("UPDATE kabipay_ops.organization_company SET status='ACTIVE'");
 await pool.query("INSERT INTO kabipay_ops.organization_company_capture(organization_id,source_tenant_id,acceptance_status,membership_id,membership_version,bootstrap_complete,topic,expected_partitions,history_from,source_bootstrap_through,accepted_at,reconciliation_sha256,accepted_by_principal_id) VALUES($1,$2,'ACCEPTED',$3,2,true,$4,ARRAY[0],now()-interval '1 day',now(),now(),repeat('a',64),$5)",[org,tenant,member,topic,principal]);
 await pool.query("INSERT INTO kabipay_ops.organization_capture_marker VALUES($1,$2,$3,$4,2,$5,now(),'0/100','0/101',true,ARRAY[0],clock_timestamp())",[marker,org,tenant,member,topic]);
 for(const role of ['inbox','history','reporting'])await pool.query('INSERT INTO kabipay_ops.organization_capture_barrier(marker_id,consumer_role,partition_id,kafka_offset) VALUES($1,$2,0,100)',[marker,role]);
 async function reconcile(role,override={}){const k=await row(role);return pool.query('SELECT kabipay_ops.organization_reconcile_historical_checkpoint($1,$2,$3,$4,0,$5,$6,$7,$8,$9)',[role,org,tenant,topic,k.kafka_offset,k.acknowledged_at,override.marker??marker,override.review??'b'.repeat(64),principal]);}
 await assert.rejects(reconcile('inbox'),/HISTORICAL_CHECKPOINT_REQUIRED/);await assert.rejects(reconcile('history'),/HISTORICAL_CHECKPOINT_REQUIRED/);await assert.rejects(reconcile('reporting',{review:'invalid'}),/QUARANTINE_REVIEW_REQUIRED/);await assert.rejects(reconcile('reporting',{marker:randomUUID()}),/CURRENT_CAPTURE_BARRIERS_REQUIRED/);
 assert.equal((await pool.query("SELECT kabipay_ops.organization_capture_healthy($1,$2,ARRAY['inbox','history','reporting']) AS healthy",[org,tenant])).rows[0].healthy,false);
 // Explicit synthetic setup resets the two genuine fault rows, not the recovery function.
 await pool.query("UPDATE kabipay_ops.organization_consumer_checkpoint SET state='READY',detail_code=NULL WHERE consumer_role IN ('inbox','history')");
 await reconcile('reporting');assert.equal((await row('reporting')).state,'READY');assert.equal((await row('reporting')).detail_code,null);assert.equal((await pool.query('SELECT count(*)::int AS n FROM kabipay_ops.organization_checkpoint_reconciliation')).rows[0].n,1);
 assert.equal((await pool.query("SELECT kabipay_ops.organization_capture_healthy($1,$2,ARRAY['inbox','history','reporting']) AS healthy",[org,tenant])).rows[0].healthy,true);
 console.log(JSON.stringify({result:'PASS',historical_status_not_data_loss:true,legacy_recovery_explicit_and_audited:true,real_partial_and_quarantine_preserved:true,exact_epoch_classification:true,resumed_all_sink_coverage_healthy:true}));
}finally{await pool?.end();for(let i=0;i<40;i++){if((await admin.query('SELECT count(*)::int AS n FROM pg_stat_activity WHERE datname=$1',[db])).rows[0].n===0)break;await new Promise(r=>setTimeout(r,25));}await admin.query(`DROP DATABASE IF EXISTS ${db}`);await admin.end();}
