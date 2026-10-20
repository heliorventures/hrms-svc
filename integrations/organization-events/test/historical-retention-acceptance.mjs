import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import {readFileSync} from 'node:fs';
import pg from 'pg';
import {Kafka,logLevel} from 'kafkajs';
import {ProjectionStore} from '../src/store.mjs';
import {ClickHouseHistory} from '../src/history.mjs';
import {consumeFixtureRecord,installConsumerSchema} from './consumer-fixture.mjs';
import {processConsumerRecord} from '../src/consumers.mjs';
if(process.env.ORGANIZATION_SYNTHETIC_ACCEPTANCE!=='true')throw Error('SYNTHETIC_OPT_IN_REQUIRED');
process.env.ALLOW_ISOLATED_PLAINTEXT='true';
const suffix=randomUUID().replaceAll('-','').slice(0,12),metadataName=`historical_metadata_${suffix}`,sourceName=`historical_source_${suffix}`,role=`historical_worker_${suffix}`;
const base='postgres://acceptance:synthetic-only-password@127.0.0.1:15436/';
const adminDb=new pg.Client({connectionString:base+'organization_acceptance'}),metadata=new pg.Client({connectionString:base+metadataName}),source=new pg.Client({connectionString:base+sourceName});
const pool=new pg.Pool({connectionString:`postgres://${role}:synthetic-history-worker@127.0.0.1:15436/${metadataName}`,max:2});
const history=new ClickHouseHistory({url:'http://127.0.0.1:18123',user:'acceptance',password:'synthetic-only-password'});
const broken=new ClickHouseHistory({url:'http://127.0.0.1:18123',user:'synthetic-nonexistent-history',password:'synthetic-invalid'});
const kafka=new Kafka({clientId:'synthetic-historical-retention',brokers:['127.0.0.1:19092'],logLevel:logLevel.NOTHING});
const group=`historical-retention-${suffix}`,admin=kafka.admin(),producer=kafka.producer(),consumer=kafka.consumer({groupId:group});
const topic=`organization.company.synthetic.history.${suffix}`,org=randomUUID(),pending=randomUUID(),unknown=randomUUID(),tenant=randomUUID(),member=randomUUID(),employee=randomUUID();
const quarantineTopic=`organization.company.synthetic.history.quarantine.${suffix}`;
const bindings={[topic]:{tenant_id:tenant,organization_ids:[org,pending,unknown]}};
let createdMetadata=false,createdSource=false,createdRole=false,timer;
function event(event_type,aggregate_type,aggregate_id,payload,organization_id=org){return {schema_version:1,event_id:randomUUID(),event_type,organization_id,source_tenant_id:tenant,aggregate_type,aggregate_id,aggregate_version:1,occurred_at:'2026-10-01T00:00:00Z',deleted:false,payload,actor:{kind:'system'}};}
async function sourceCommit(e){await source.query('BEGIN');await source.query('INSERT INTO source_business_receipt(id) VALUES($1)',[e.event_id]);await source.query('INSERT INTO company_event_outbox(id,event_envelope) VALUES($1,$2)',[e.event_id,e]);await source.query('COMMIT');await producer.send({topic,messages:[{key:e.aggregate_id,value:JSON.stringify(e)}]});}
try{
 await adminDb.connect();await adminDb.query(`CREATE DATABASE ${metadataName}`);createdMetadata=true;await adminDb.query(`CREATE DATABASE ${sourceName}`);createdSource=true;
 await metadata.connect();await source.connect();
 await source.query('CREATE TABLE source_business_receipt(id uuid PRIMARY KEY);CREATE TABLE company_event_outbox(id uuid PRIMARY KEY,event_envelope jsonb NOT NULL)');
 await metadata.query("CREATE SCHEMA kabipay_ops;CREATE TABLE kabipay_ops.organization(id uuid PRIMARY KEY,status text NOT NULL DEFAULT 'ACTIVE');CREATE TABLE kabipay_ops.tenant(id uuid PRIMARY KEY,status text NOT NULL DEFAULT 'ACTIVE',is_deleted boolean NOT NULL DEFAULT false,private_configuration text);CREATE TABLE kabipay_ops.principal(id uuid PRIMARY KEY);CREATE TABLE kabipay_ops.organization_company(id uuid PRIMARY KEY,organization_id uuid NOT NULL,tenant_id uuid NOT NULL,status text NOT NULL DEFAULT 'ACTIVE',version bigint NOT NULL DEFAULT 1,installed_version bigint DEFAULT 1,runtime_status text NOT NULL DEFAULT 'ACTIVE',effective_from timestamptz NOT NULL DEFAULT now(),effective_to timestamptz)");
 const xml=readFileSync(new URL('../../../../hrms-database/changelog/migrations/0116_organization_projections/organization_projections.xml',import.meta.url),'utf8');await metadata.query(xml.match(/<!\[CDATA\[([\s\S]*?)\]\]>/)[1]);await installConsumerSchema(metadata);
 await metadata.query('INSERT INTO kabipay_ops.organization(id) VALUES($1),($2)',[org,pending]);await metadata.query('INSERT INTO kabipay_ops.tenant(id) VALUES($1)',[tenant]);
 await metadata.query("INSERT INTO kabipay_ops.organization_company(id,organization_id,tenant_id,status) VALUES($1,$2,$3,'ACTIVE'),($4,$5,$3,'PENDING')",[member,org,tenant,randomUUID(),pending]);
 await adminDb.query(`CREATE ROLE ${role} LOGIN PASSWORD 'synthetic-history-worker'`);createdRole=true;
 await metadata.query(`GRANT USAGE ON SCHEMA kabipay_ops TO ${role};GRANT EXECUTE ON FUNCTION kabipay_ops.organization_capture_member_classification(uuid,uuid),kabipay_ops.organization_capture_member_active(uuid,uuid) TO ${role};GRANT SELECT,INSERT,UPDATE ON kabipay_ops.organization_inbox_projection,kabipay_ops.organization_directory_projection,kabipay_ops.organization_projection_fence,kabipay_ops.organization_projection_watermark,kabipay_ops.organization_consumer_checkpoint,kabipay_ops.organization_consumer_identity TO ${role}`);
 for(const table of ['organization_consumer_checkpoint','organization_consumer_identity'])await metadata.query(`CREATE POLICY synthetic_worker ON kabipay_ops.${table} TO ${role} USING (true) WITH CHECK (true)`);
 await assert.rejects(pool.query('SELECT private_configuration FROM kabipay_ops.tenant'),e=>e.code==='42501');
 await assert.rejects(pool.query("UPDATE kabipay_ops.organization_company SET status='ACTIVE'"),e=>e.code==='42501');
 for(const sql of readFileSync(new URL('../../../../hrms-clickhouse/migrations/0001-company-history.sql',import.meta.url),'utf8').split(';').map(s=>s.trim()).filter(Boolean))await history.query(sql);
 await admin.connect();await admin.createTopics({waitForLeaders:true,topics:[topic,quarantineTopic].map(topic=>({topic,numPartitions:1,replicationFactor:1}))});await producer.connect();
 const approval=event('approval.changed','leave_request',randomUUID(),{resource_type:'leave_request',resource_id:'',employee_id:employee,employee_display_name:'Synthetic employee',status:'PENDING',summary:'Leave request'});approval.payload.resource_id=approval.aggregate_id;
 assert.equal((await metadata.query('SELECT kabipay_ops.organization_capture_member_classification($1,$2) AS state',[org,tenant])).rows[0].state,'ACTIVE');
 await sourceCommit(approval); // Committed while ACTIVE, queued before suspension.
 await metadata.query("UPDATE kabipay_ops.organization_company SET status='SUSPENDED' WHERE id=$1",[member]);
 const directory=event('directory.changed','employee',employee,{employee_id:employee,display_name:'Synthetic employee',employment_status:'ACTIVE'});
 const nonePending=event('directory.changed','employee',randomUUID(),{},pending);nonePending.payload={employee_id:nonePending.aggregate_id,display_name:'No accepted membership',employment_status:'ACTIVE'};
 const noneUnknown=event('directory.changed','employee',randomUUID(),{},unknown);noneUnknown.payload={employee_id:noneUnknown.aggregate_id,display_name:'No organization',employment_status:'ACTIVE'};
 const quarantine=[];let count=0,resolve,reject;const done=new Promise((r,j)=>{resolve=r;reject=j;});timer=setTimeout(()=>reject(Error('HISTORICAL_ACCEPTANCE_TIMEOUT')),60000);
 await consumer.connect();await consumer.subscribe({topic,fromBeginning:true});await consumer.run({autoCommit:false,eachMessage:async({topic,partition,message})=>{
  try{
   const args={raw:message.value,topic,partition,offset:message.offset,bindings,store:new ProjectionStore(pool),history,quarantine:async m=>{await producer.send({topic:quarantineTopic,acks:-1,messages:[{value:JSON.stringify(m)}]});quarantine.push(m);}};
   if(count===0){await assert.rejects(processConsumerRecord({...args,history:broken,role:'history'}));assert.equal(Number((await pool.query('SELECT count(*) AS n FROM kabipay_ops.organization_projection_watermark')).rows[0].n),0);}
   const result=await consumeFixtureRecord(args);
   if(count<2)assert.equal(result.historical,true);else assert.equal(result.discarded,true);
   await consumer.commitOffsets([{topic,partition,offset:(BigInt(message.offset)+1n).toString()}]);count++;
   if(count===1){await metadata.query("UPDATE kabipay_ops.organization_company SET status='ACTIVE' WHERE id=$1",[member]);await sourceCommit(directory);await metadata.query("UPDATE kabipay_ops.organization_company SET status='DETACHED',runtime_status='DETACHED',effective_to=now() WHERE id=$1",[member]);await sourceCommit(nonePending);await sourceCommit(noneUnknown);}
   if(count===4)resolve();
  }catch(error){reject(error);throw error;}
 }});await done;clearTimeout(timer);
 const rows=(await history.query('SELECT event_type FROM organization.business_history FINAL WHERE source_tenant_id={tenant:UUID} FORMAT JSONEachRow',{tenant})).trim().split('\n').map(JSON.parse);assert.equal(rows.length,2);assert.deepEqual(rows.map(r=>r.event_type).sort(),['approval.changed','directory.changed']);
 for(const table of ['organization_inbox_projection','organization_directory_projection','organization_projection_fence'])assert.equal(Number((await pool.query(`SELECT count(*) AS n FROM kabipay_ops.${table}`)).rows[0].n),0,'no current projection/fence retained for inactive pairs');
 const marker=(await pool.query('SELECT state,detail_code FROM kabipay_ops.organization_consumer_checkpoint')).rows;assert.equal(marker.length,3);assert(marker.every(row=>row.state==='READY'&&row.detail_code===null));
 assert.equal((await pool.query('SELECT kabipay_ops.organization_capture_member_active($1,$2) AS active',[org,tenant])).rows[0].active,false);
 assert.equal((await metadata.query("SELECT count(*)::int AS n FROM kabipay_ops.organization_company WHERE organization_id=$1 AND tenant_id=$2 AND status='ACTIVE' AND runtime_status='ACTIVE' AND installed_version=version AND effective_from<=now() AND (effective_to IS NULL OR now()<effective_to)",[org,tenant])).rows[0].n,0,'inactive API membership eligibility remains false');
 assert.equal(quarantine.length,6);assert(quarantine.every(q=>q.code==='MEMBERSHIP_UNRECOGNIZED'&&!Object.hasOwn(q,'payload')));
 assert.equal((await admin.fetchOffsets({groupId:group,topics:[topic]}))[0].partitions[0].offset,'4');
 assert.equal((await source.query('SELECT count(*)::int AS n FROM source_business_receipt')).rows[0].n,4);
 console.log(JSON.stringify({result:'PASS',actual0116_restricted_classifier:true,separate_source_metadata_databases:true,queued_while_active_consumed_suspended_and_detached:true,retained_history_events:2,current_projection_rows:0,current_fence_rows:0,unknown_and_pending_quarantined:6,durable_quarantine_before_broker_offset:'4',old_fact_time_not_filtered:true,failed_history_sink_no_watermark:true,inactive_membership_eligibility:false,independent_historical_checkpoints:'READY',source_receipts_preserved:4}));
}finally{
 clearTimeout(timer);await consumer.disconnect().catch(()=>{});await producer.disconnect().catch(()=>{});await admin.disconnect().catch(()=>{});await pool.end().catch(()=>{});await metadata.end().catch(()=>{});await source.end().catch(()=>{});
 if(createdMetadata)await adminDb.query(`DROP DATABASE ${metadataName}`);if(createdSource)await adminDb.query(`DROP DATABASE ${sourceName}`);if(createdRole)await adminDb.query(`DROP ROLE ${role}`);await adminDb.end().catch(()=>{});
}
