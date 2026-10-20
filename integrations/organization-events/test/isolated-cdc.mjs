import assert from 'node:assert/strict';
import pg from 'pg';
import {randomUUID} from 'node:crypto';
import {Kafka,logLevel} from 'kafkajs';
import {ProjectionStore} from '../src/store.mjs';
import {ClickHouseHistory} from '../src/history.mjs';
import {consumeFixtureRecord,installProjectionFixture} from './consumer-fixture.mjs';
import {progress} from './report-fixture.mjs';
import {fixtureConnectors,fixtureCompanyTopics,fixtureCompanyTopic,fixtureCompanyRoutingDdl,isolatedConnector,withFixtureCleanup} from './kafka-fixture.mjs';
if(process.env.ORGANIZATION_SYNTHETIC_ACCEPTANCE!=='true')throw Error('EXPLICIT_SYNTHETIC_OPT_IN_REQUIRED');
process.env.ALLOW_ISOLATED_PLAINTEXT='true';
const tag=randomUUID().replaceAll('-',''),database='isolated_cdc_'+tag,org=randomUUID(),tenant=randomUUID(),id=randomUUID(),eventIds=new Map();
const rendered=isolatedConnector(fixtureConnectors({tenant,sourceDatabase:database})[0],'synthetic-cdc-password');
const config=rendered.config,topic=fixtureCompanyTopic(config),connector=rendered.name,cdcRole=config['database.user'],slot=config['slot.name'];
const adminDb=new pg.Pool({connectionString:'postgres://acceptance:synthetic-only-password@127.0.0.1:15436/postgres'});
const pool=new pg.Pool({connectionString:'postgres://acceptance:synthetic-only-password@127.0.0.1:15436/'+database,options:'-c search_path=company_fixture'});let created=false,registered=false,roleCreated=false;
const history=new ClickHouseHistory({url:'http://127.0.0.1:18123',user:'acceptance',password:'synthetic-only-password'});
const kafka=new Kafka({clientId:'synthetic-cdc',brokers:['127.0.0.1:19092'],logLevel:logLevel.NOTHING});
const admin=kafka.admin(),consumer=kafka.consumer({groupId:'synthetic-cdc-'+Date.now()});
let timer;
function event(version){return {schema_version:1,event_id:eventIds.get(version)??(eventIds.set(version,randomUUID()),eventIds.get(version)),event_type:'timesheet.changed',organization_id:org,source_tenant_id:tenant,aggregate_type:'timesheet_entry',aggregate_id:id,aggregate_version:version,occurred_at:'2026-10-19T01:00:00Z',deleted:false,payload:{employee_id:id,work_date:'2026-10-19',logged_seconds:version===1?3600:1800,approved_seconds:1800}};}
async function insert(client,e){await client.query('INSERT INTO company_fixture.company_event_outbox(id,source_tenant_id,organization_id,event_type,aggregate_type,aggregate_id,aggregate_version,event_envelope) VALUES($1,$2,$3,$4,$5,$6,$7,$8)',[e.event_id,e.source_tenant_id,e.organization_id,e.event_type,e.aggregate_type,e.aggregate_id,e.aggregate_version,e]);}
await withFixtureCleanup(async()=>{
 await adminDb.query(`CREATE DATABASE ${database}`);created=true;await installProjectionFixture(pool,{org,tenant,id});
 let ready=false;for(let attempt=0;attempt<30;attempt++){try{ready=(await fetch('http://127.0.0.1:18083/connectors',{signal:AbortSignal.timeout(2000)})).ok;if(ready)break;}catch{}await new Promise(r=>setTimeout(r,1000));}assert(ready,'synthetic Connect did not become ready');
 await pool.query(`CREATE SCHEMA company_fixture; CREATE TABLE IF NOT EXISTS company_fixture.synthetic_worklog(id uuid PRIMARY KEY,seconds bigint NOT NULL);
 CREATE TABLE IF NOT EXISTS company_fixture.company_event_outbox(id uuid PRIMARY KEY,source_tenant_id uuid NOT NULL,organization_id uuid NOT NULL,event_type text NOT NULL,aggregate_type text NOT NULL,aggregate_id uuid NOT NULL,aggregate_version bigint NOT NULL,event_envelope jsonb NOT NULL,source_position bigserial NOT NULL,created_at timestamptz NOT NULL DEFAULT now());
 CREATE TABLE company_fixture.organization_capture_marker(marker_id uuid PRIMARY KEY);
 CREATE ROLE ${cdcRole} LOGIN REPLICATION PASSWORD 'synthetic-cdc-password';
 GRANT CONNECT ON DATABASE ${database} TO ${cdcRole};
 GRANT USAGE ON SCHEMA company_fixture TO ${cdcRole};GRANT SELECT ON company_fixture.company_event_outbox,company_fixture.organization_capture_marker TO ${cdcRole};
 DROP PUBLICATION IF EXISTS organization_company_outbox; CREATE PUBLICATION organization_company_outbox FOR TABLE company_fixture.company_event_outbox,company_fixture.organization_capture_marker;`);
 roleCreated=true;
 await pool.query(fixtureCompanyRoutingDdl('company_fixture'));
 // Snapshot baseline is itself atomic with its source write.
 const client=await pool.connect();try{await client.query('BEGIN');await client.query('INSERT INTO company_fixture.synthetic_worklog VALUES($1,3600)',[id]);await insert(client,event(1));await client.query('COMMIT');}finally{client.release();}
 await admin.connect();await admin.createTopics({waitForLeaders:true,topics:fixtureCompanyTopics({tenant,sourceDatabase:database}).map(topic=>({topic,numPartitions:1,replicationFactor:1}))});

 const response=await fetch('http://127.0.0.1:18083/connectors',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({name:connector,config}),signal:AbortSignal.timeout(15000)});if(!response.ok)throw Error('CONNECT_REGISTER_'+response.status+':'+await response.text());
 registered=true;let count=0,resolve,reject;const done=new Promise((r,j)=>{resolve=r;reject=j;});timer=setTimeout(()=>reject(Error('CDC_TIMEOUT')),90000);
 await consumer.connect();await consumer.subscribe({topic,fromBeginning:true});await consumer.run({autoCommit:false,eachMessage:async({topic,partition,message})=>{
  try{assert.equal(message.key.toString(),id);await consumeFixtureRecord({raw:message.value,topic,partition,offset:message.offset,bindings:{[topic]:{organization_id:org,tenant_id:tenant}},store:new ProjectionStore(pool),history,quarantine:async()=>{throw Error('UNEXPECTED_QUARANTINE');}});await consumer.commitOffsets([{topic,partition,offset:(BigInt(message.offset)+1n).toString()}]);count++;
   if(count===1){const c=await pool.connect();try{await c.query('BEGIN');await c.query('UPDATE company_fixture.synthetic_worklog SET seconds=99999 WHERE id=$1',[id]);await insert(c,event(3));await c.query('ROLLBACK');await c.query('BEGIN');await c.query('UPDATE company_fixture.synthetic_worklog SET seconds=1800 WHERE id=$1',[id]);await insert(c,event(2));await c.query('COMMIT');}finally{c.release();}}
   if(count===2)resolve();
  }catch(e){reject(e);throw e;}
 }});await done;
 const report=await progress(history,{organizationId:org,tenantIds:[tenant],from:'2026-10-19',to:'2026-10-19'});assert.equal(report.rows[0].logged_seconds,'1800');
 assert.equal((await pool.query('SELECT count(*)::int AS n FROM company_fixture.company_event_outbox')).rows[0].n,2);
 assert.equal(Number((await pool.query('SELECT seconds FROM company_fixture.synthetic_worklog WHERE id=$1',[id])).rows[0].seconds),1800);
 const denied=new pg.Client({connectionString:'postgres://'+cdcRole+':synthetic-cdc-password@127.0.0.1:15436/'+database});await denied.connect();try{await assert.rejects(denied.query('SELECT * FROM company_fixture.synthetic_worklog'),e=>e.code==='42501');}finally{await denied.end();}
 console.log(JSON.stringify({result:'PASS',real_debezium_cdc_events:count,snapshot_then_transactional_correction:true,rolled_back_outbox_not_visible:true,restricted_cdc_role_business_read_denied:true,corrected_logged_seconds:report.rows[0].logged_seconds}));
},async()=>{clearTimeout(timer);await consumer.disconnect();await admin.disconnect();if(registered)await fetch('http://127.0.0.1:18083/connectors/'+connector,{method:'DELETE',signal:AbortSignal.timeout(15000)});await pool.end();if(registered){let detached=false;for(let attempt=0;attempt<100;attempt++){const slots=(await adminDb.query('SELECT active FROM pg_replication_slots WHERE slot_name=$1 AND database=$2',[slot,database])).rows;if(!slots.length){detached=true;break;}if(!slots[0].active){await adminDb.query('SELECT pg_drop_replication_slot($1)',[slot]);detached=true;break;}await new Promise(resolve=>setTimeout(resolve,100));}if(!detached)throw Error('SYNTHETIC_SLOT_STILL_ACTIVE');}if(created)await adminDb.query(`DROP DATABASE ${database}`);if(roleCreated)await adminDb.query(`DROP ROLE ${cdcRole}`);await adminDb.end();});
