// Explicit opt-in integration acceptance against acceptance.compose.yaml ONLY.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import pg from 'pg';
import {randomUUID} from 'node:crypto';
import {Kafka,logLevel} from 'kafkajs';
import {ProjectionStore} from '../src/store.mjs';
import {ClickHouseHistory} from '../src/history.mjs';
import {consumeFixtureRecord,installProjectionFixture} from './consumer-fixture.mjs';
import {processConsumerRecord} from '../src/consumers.mjs';
import {progress} from './report-fixture.mjs';
if(process.env.ORGANIZATION_SYNTHETIC_ACCEPTANCE!=='true')throw Error('EXPLICIT_SYNTHETIC_OPT_IN_REQUIRED');
process.env.ALLOW_ISOLATED_PLAINTEXT='true';
const org=randomUUID(),tenant=randomUUID(),id=randomUUID(),database='isolated_acceptance_'+randomUUID().replaceAll('-','');
const topic='organization.synthetic.acceptance.'+database;
const adminDb=new pg.Pool({connectionString:'postgres://acceptance:synthetic-only-password@127.0.0.1:15436/postgres'});
const pool=new pg.Pool({connectionString:'postgres://acceptance:synthetic-only-password@127.0.0.1:15436/'+database});let created=false;
const history=new ClickHouseHistory({url:'http://127.0.0.1:18123',user:'acceptance',password:'synthetic-only-password'});
const kafka=new Kafka({clientId:'synthetic-acceptance',brokers:['127.0.0.1:19092'],logLevel:logLevel.NOTHING});
const admin=kafka.admin(),producer=kafka.producer(),consumer=kafka.consumer({groupId:'organization-synthetic-acceptance-'+Date.now()});
try {
 await adminDb.query(`CREATE DATABASE ${database}`);created=true;await installProjectionFixture(pool,{org,tenant,id});
 for(const sql of readFileSync(new URL('../../../../hrms-clickhouse/migrations/0001-company-history.sql',import.meta.url),'utf8').split(';').map(s=>s.trim()).filter(Boolean)) await history.query(sql);
 await admin.connect();await admin.createTopics({topics:[{topic,numPartitions:1,replicationFactor:1},{topic:'organization.synthetic.quarantine',numPartitions:1,replicationFactor:1}],waitForLeaders:true});
 await producer.connect();await consumer.connect();await consumer.subscribe({topic,fromBeginning:true});
 const events=[],eventIds=new Map();
 function event(version,extra={}) {return {schema_version:1,event_id:eventIds.get(version)??(eventIds.set(version,randomUUID()),eventIds.get(version)),event_type:'timesheet.changed',organization_id:org,source_tenant_id:tenant,aggregate_type:'timesheet_entry',aggregate_id:id,aggregate_version:version,occurred_at:'2026-10-19T00:00:00Z',deleted:false,payload:{employee_id:id,work_date:'2026-10-18',logged_seconds:3600,approved_seconds:0},...extra};}
 const corrected=event(2,{payload:{employee_id:id,work_date:'2026-10-19',logged_seconds:1800,approved_seconds:1800}});
 events.push(event(1),event(1),corrected,event(1),event(3,{deleted:true}),{bad:'private-do-not-copy'});
 let count=0;const quarantine=[];let complete,failed;const done=new Promise((resolve,reject)=>{complete=resolve;failed=reject;});
 const timer=setTimeout(()=>failed(Error('CONSUMER_TIMEOUT')),60000);
 await consumer.run({autoCommit:false,eachMessage:async({topic,partition,message})=>{
  try {await consumeFixtureRecord({raw:message.value,topic,partition,offset:message.offset,bindings:{[topic]:{organization_id:org,tenant_id:tenant}},store:new ProjectionStore(pool),history,quarantine:async m=>quarantine.push(m)});await consumer.commitOffsets([{topic,partition,offset:(BigInt(message.offset)+1n).toString()}]);count++;
   if(count===4){const correctedReport=await progress(history,{organizationId:org,tenantIds:[tenant],from:'2026-10-18',to:'2026-10-19'});assert.equal(correctedReport.rows.length,1);assert.equal(correctedReport.rows[0].work_date,'2026-10-19');assert.equal(correctedReport.rows[0].logged_seconds,'1800');assert.equal(correctedReport.rows[0].approved_seconds,'1800');}
   if(count===events.length)complete();}catch(e){failed(e);throw e;}
 }});
 await producer.send({topic,messages:events.map(e=>({key:id,value:JSON.stringify(e)}))});
 await done;clearTimeout(timer);
 const report=await progress(history,{organizationId:org,tenantIds:[tenant],from:'2026-10-18',to:'2026-10-19'});assert.deepEqual(report.rows,[]);
 const rows=JSON.parse(await history.query('SELECT count() AS n FROM organization.business_history FINAL WHERE source_tenant_id={tenant:UUID} FORMAT JSONEachRow',{tenant}));assert.equal(Number(rows.n),3);
 const fence=(await pool.query('SELECT source_version FROM kabipay_ops.organization_projection_fence')).rows;assert.equal(Number(fence[0].source_version),3);
 const watermark=(await pool.query('SELECT state,kafka_offset FROM kabipay_ops.organization_projection_watermark')).rows;assert.equal(watermark[0].state,'READY');assert.equal((await pool.query("SELECT count(*)::int AS n FROM kabipay_ops.organization_consumer_checkpoint WHERE state='PARTIAL'")).rows[0].n,3);assert.equal(quarantine.length,3);assert(!JSON.stringify(quarantine).includes('private-do-not-copy'));
 // An actual ClickHouse authentication failure must leave PG fence/offset intact.
 const broken=new ClickHouseHistory({url:'http://127.0.0.1:18123',user:'does_not_exist',password:'synthetic-only-password'});
 await assert.rejects(processConsumerRecord({role:'history',raw:Buffer.from(JSON.stringify(event(4))),topic,partition:0,offset:'6',bindings:{[topic]:{organization_id:org,tenant_id:tenant}},store:new ProjectionStore(pool),history:broken,quarantine:async m=>quarantine.push(m)}));
 assert.equal(Number((await pool.query('SELECT source_version FROM kabipay_ops.organization_projection_fence')).rows[0].source_version),3);
 assert.equal((await pool.query('SELECT kafka_offset FROM kabipay_ops.organization_projection_watermark')).rows[0].kafka_offset,watermark[0].kafka_offset);
 // Accepted inactive membership keeps delayed history, without current projection.
 await pool.query("UPDATE kabipay_ops.organization_company SET status='SUSPENDED'");
 const discarded=await consumeFixtureRecord({raw:Buffer.from(JSON.stringify(event(4))),topic,partition:0,offset:'6',bindings:{[topic]:{organization_id:org,tenant_id:tenant}},store:new ProjectionStore(pool),history,quarantine:async m=>quarantine.push(m)});assert.equal(discarded.historical,true);
 assert.equal(Number(JSON.parse(await history.query('SELECT count() AS n FROM organization.business_history FINAL WHERE source_tenant_id={tenant:UUID} FORMAT JSONEachRow',{tenant})).n),4);
 await pool.query("UPDATE kabipay_ops.organization_company SET status='ACTIVE'");
 console.log(JSON.stringify({result:'PASS',broker_records:count,logical_history_events:Number(rows.n),latest_version:3,tombstone_excluded:true,quarantine_metadata_only:true,watermark:'READY',independent_quarantine_checkpoints:3}));
} finally {await consumer.disconnect();await producer.disconnect();await admin.disconnect();await pool.end();if(created)await adminDb.query(`DROP DATABASE ${database}`);await adminDb.end();}
