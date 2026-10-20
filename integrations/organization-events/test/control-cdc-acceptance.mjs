import {fixtureConnectors,connectorHeartbeatTopic,fixtureControlTopic,fixtureControlRoutingDdl} from './kafka-fixture.mjs';
import assert from 'node:assert/strict';
import {readFileSync,mkdtempSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {randomUUID} from 'node:crypto';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import pg from 'pg';
import {Kafka,logLevel} from 'kafkajs';
import {ControlHttp} from '../src/control-http.mjs';
if(process.env.ORGANIZATION_SYNTHETIC_ACCEPTANCE!=='true')throw Error('SYNTHETIC_OPT_IN_REQUIRED');
process.env.ALLOW_ISOLATED_PLAINTEXT='true';
const suffix=randomUUID().replaceAll('-','').slice(0,12),database=`control_history_${suffix}`,role=`control_cdc_${suffix}`,slot=`control_history_${suffix}`,connector=`control-history-${suffix}`;
const binding={topic:`organization.control.metadata.synthetic.${suffix}`,database,schema:'kabipay_ops',connector_name:`control_metadata_${suffix}`};
const quarantineTopic=`organization.control.quarantine.${suffix}`,group=`control-history-${suffix}`;
const chWorker=`control_worker_${suffix}`,chReader=`control_reader_${suffix}`,chPolicy=`control_nonnull_${suffix}`;
const base='postgres://acceptance:synthetic-only-password@127.0.0.1:15436/';
const adminDb=new pg.Client({connectionString:base+'organization_acceptance'}),db=new pg.Client({connectionString:base+database});
const http=new ControlHttp({url:'http://127.0.0.1:18123',user:'acceptance',password:'synthetic-only-password'});
const kafka=new Kafka({clientId:'synthetic-control-acceptance',brokers:['127.0.0.1:19092'],logLevel:logLevel.NOTHING});
const admin=kafka.admin(),producer=kafka.producer(),tap=kafka.consumer({groupId:`control-tap-${suffix}`});
const org=randomUUID(),tenant=randomUUID(),principal=randomUUID(),user=randomUUID(),aggregate=randomUUID(),member=randomUUID();
const directory=mkdtempSync(join(tmpdir(),'kabipay-control-'));
let worker,workerOutput='',created=false,tapMessages=[],failure,stage='setup',failSink=false,expectSinkFailure=false;
const proxy=createServer(async(req,res)=>{
 const url=new URL(req.url,'http://127.0.0.1');
 if(failSink&&url.searchParams.get('query')?.startsWith('INSERT INTO organization.control_history')){res.writeHead(503);res.end();return;}
 try{const chunks=[];for await(const chunk of req)chunks.push(chunk);const target='http://127.0.0.1:18123'+req.url;const result=await fetch(target,{method:'POST',headers:{'X-ClickHouse-User':req.headers['x-clickhouse-user'],'X-ClickHouse-Key':req.headers['x-clickhouse-key']},body:Buffer.concat(chunks),signal:AbortSignal.timeout(15000)});res.writeHead(result.status);res.end(await result.text());}catch{res.writeHead(503);res.end();}
});
await new Promise(r=>proxy.listen(0,'127.0.0.1',r));
async function until(fn,description,timeout=90000){const started=Date.now();for(;;){if(failure)throw failure;if(await fn())return;if(Date.now()-started>timeout)throw Error(`TIMEOUT_${description}`);await new Promise(r=>setTimeout(r,300));}}
async function count(){const text=await http.query('SELECT count() AS n FROM organization.control_history FINAL WHERE metadata_source={source:String} FORMAT JSONEachRow',{source:binding.connector_name});return Number(JSON.parse(text).n);}
const rootStore=readFileSync(new URL('../../../crates/kabipay-organization/src/store.rs',import.meta.url),'utf8');
const sequenceSql=rootStore.match(/"(INSERT INTO kabipay_ops\.organization_outbox_sequence[^"\n]+)"/)[1];
const outboxSql=rootStore.match(/"(INSERT INTO kabipay_ops\.organization_outbox\(id,[^"\n]+)"/)[1];
async function sourceEvent(client,{eventId=randomUUID(),organization=org,aggregateId=aggregate,kind,change,configurationVersion=1}){
 const seq=(await client.query(sequenceSql,[aggregateId])).rows[0].version;
 await client.query(outboxSql,[eventId,organization,tenant,principal,aggregateId,seq,kind,{actor:{principal_id:principal,source_tenant_id:tenant,source_user_id:user,issuer:'kabipay-client'},configuration_version:configurationVersion,change}]);
 return eventId;
}
try{
 await adminDb.connect();await adminDb.query(`CREATE DATABASE ${database}`);created=true;await db.connect();
 await db.query('CREATE SCHEMA kabipay_ops; CREATE TABLE kabipay_ops.organization(id uuid PRIMARY KEY); CREATE TABLE kabipay_ops.tenant(id uuid PRIMARY KEY); CREATE TABLE kabipay_ops.principal(id uuid PRIMARY KEY); CREATE TABLE kabipay_ops.synthetic_private_secret(value text);');
 const xml=readFileSync(new URL('../../../../hrms-database/changelog/migrations/0113_organization_control_outbox/organization_control_outbox.xml',import.meta.url),'utf8');
 await db.query(xml.split('<![CDATA[')[1].split(']]>')[0]);
 const execution=readFileSync(new URL('../../../../hrms-database/changelog/migrations/0114_organization_execution/organization_execution.xml',import.meta.url),'utf8');
 await db.query(fixtureControlRoutingDdl());
 await db.query(execution.match(/CREATE TABLE kabipay_ops\.organization_outbox_sequence\s*\([\s\S]*?\);/)[0]);
 await db.query('INSERT INTO kabipay_ops.organization VALUES($1)',[org]);await db.query('INSERT INTO kabipay_ops.tenant VALUES($1)',[tenant]);await db.query('INSERT INTO kabipay_ops.principal VALUES($1)',[principal]);
 await db.query('BEGIN');await sourceEvent(db,{organization:null,aggregateId:principal,kind:'identity.registered',change:{principal_id:principal,link_id:user,display_name:'Synthetic actor',status:'VERIFIED'}});await db.query('COMMIT');
 await db.query(`CREATE ROLE ${role} LOGIN REPLICATION PASSWORD 'synthetic-control-only'; GRANT CONNECT ON DATABASE ${database} TO ${role}; GRANT USAGE ON SCHEMA kabipay_ops TO ${role}; GRANT SELECT ON kabipay_ops.organization_outbox TO ${role}; CREATE PUBLICATION organization_control_outbox FOR TABLE kabipay_ops.organization_outbox;`);
 const denied=new pg.Client({connectionString:`postgres://${role}:synthetic-control-only@127.0.0.1:15436/${database}`});await denied.connect();try{await assert.rejects(denied.query('SELECT * FROM kabipay_ops.synthetic_private_secret'),e=>e.code==='42501');await assert.rejects(denied.query('UPDATE kabipay_ops.organization_outbox SET schema_version=1'),e=>e.code==='42501');}finally{await denied.end();}
 const sql=readFileSync(new URL('../../../../hrms-clickhouse/migrations/0002-control-history.sql',import.meta.url),'utf8').replace(/--[^\n]*/g,'');
 for(const statement of sql.split(';').map(s=>s.trim()).filter(Boolean))await http.query(statement);
 await http.query(`CREATE USER ${chWorker} IDENTIFIED WITH sha256_password BY 'synthetic-control-worker-only'`);
 await http.query(`CREATE USER ${chReader} IDENTIFIED WITH sha256_password BY 'synthetic-control-reader-only'`);
 await http.query(`GRANT SELECT,INSERT ON organization.control_history TO ${chWorker}`);
 await http.query(`GRANT SELECT,INSERT ON organization.control_capture_checkpoint TO ${chWorker}`);
 await http.query(`GRANT SELECT ON organization.control_history TO ${chReader}`);
 await http.query(`CREATE ROW POLICY ${chPolicy} ON organization.control_history FOR SELECT USING organization_id IS NOT NULL TO ${chReader}`);
 const template=fixtureConnectors({tenant,sourceDatabase:'unused_company_fixture',metadataDatabase:database})[1].config;
 const config={...template,'database.hostname':'postgres','database.dbname':database,'database.user':role,'database.password':'synthetic-control-only','database.sslmode':'disable','slot.name':slot,'topic.prefix':binding.connector_name,'predicates.isOutbox.pattern':`${binding.connector_name}\\.kabipay_ops\\.organization_outbox`,'transforms.outbox.route.topic.replacement':template['transforms.outbox.route.topic.replacement']+'.synthetic.'+suffix};delete config['database.sslrootcert'];assert.equal(fixtureControlTopic(config),binding.topic);
 await admin.connect();await admin.createTopics({waitForLeaders:true,topics:[binding.topic,quarantineTopic,connectorHeartbeatTopic(config)].map(topic=>({topic,numPartitions:1,replicationFactor:1}))});await producer.connect();
 await tap.connect();await tap.subscribe({topic:binding.topic,fromBeginning:true});await tap.run({eachMessage:async({message})=>{tapMessages.push(message.value);}});

 const registered=await fetch('http://127.0.0.1:18083/connectors',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({name:connector,config}),signal:AbortSignal.timeout(15000)});assert(registered.ok,`synthetic connector registration status ${registered.status}`);
 const bindingFile=join(directory,'binding.json');writeFileSync(bindingFile,JSON.stringify(binding));
 function startWorker(){worker=spawn(process.execPath,['src/control-worker.mjs'],{cwd:new URL('..',import.meta.url),env:{...process.env,CONTROL_BINDING_FILE:bindingFile,CONTROL_QUARANTINE_TOPIC:quarantineTopic,CONTROL_GROUP_ID:group,KAFKA_BROKERS:'127.0.0.1:19092',CLICKHOUSE_URL:`http://127.0.0.1:${proxy.address().port}`,CLICKHOUSE_USER:chWorker,CLICKHOUSE_PASSWORD:'synthetic-control-worker-only'},stdio:['ignore','pipe','pipe']});
 worker.stdout.on('data',b=>{workerOutput=(workerOutput+b.toString()).slice(-10000);});worker.stderr.on('data',b=>{workerOutput=(workerOutput+b.toString()).slice(-10000);});worker.on('error',()=>failure=Error('WORKER_START'));worker.on('exit',code=>{if(!expectSinkFailure&&code&&code!==143)failure=Error('WORKER_EXIT');});}
 startWorker();
 stage='snapshot';await until(async()=>await count()===1&&tapMessages.length===1,'snapshot');
 console.log(JSON.stringify({stage:'SNAPSHOT_ACCEPTED'}));
 await db.query('BEGIN');await sourceEvent(db,{kind:'relationship.proposed',change:{id:aggregate,subject:principal,manager:user,graph_version:2}});await sourceEvent(db,{kind:'relationship.installed',change:{id:aggregate,membership_id:member,version:1,graph_version:2}});await db.query('COMMIT');
 await db.query('BEGIN');await sourceEvent(db,{kind:'relationship.ended',change:{id:aggregate,status:'ENDED',graph_version:3}});await db.query('ROLLBACK');
 stage='streamed';await until(async()=>await count()===3&&tapMessages.length===3,'streamed');
 console.log(JSON.stringify({stage:'STREAM_ACCEPTED'}));
 stage='verify_versions';const rows=(await http.query('SELECT event_type,toString(aggregate_version) AS sequence,toString(configuration_version) AS revision,organization_id FROM organization.control_history FINAL WHERE metadata_source={source:String} ORDER BY aggregate_id,aggregate_version FORMAT JSONEachRow',{source:binding.connector_name})).trim().split('\n').map(JSON.parse);
 assert.equal(rows.find(r=>r.event_type==='identity.registered').organization_id,null);
 const proposed=rows.find(r=>r.event_type==='relationship.proposed'),installed=rows.find(r=>r.event_type==='relationship.installed');assert.equal(proposed.sequence,'1');assert.equal(installed.sequence,'2');assert.equal(proposed.revision,'1');assert.equal(installed.revision,'1');
 stage='sink_failure';failSink=true;expectSinkFailure=true;
 await db.query('BEGIN');await sourceEvent(db,{kind:'relationship.ended',change:{id:aggregate,status:'ENDED',graph_version:3},configurationVersion:2});await db.query('COMMIT');
 await until(async()=>worker.exitCode!==null,'failed_sink_shutdown');
 assert.equal((await admin.fetchOffsets({groupId:group,topics:[binding.topic]}))[0].partitions[0].offset,'3','failed required sink must not advance broker offset');
 assert.equal(await count(),3);failSink=false;expectSinkFailure=false;startWorker();
 await until(async()=>await count()===4,'sink_recovery');
 let conflict=JSON.parse(tapMessages[0]);const stringWrapped=typeof conflict==='string';if(stringWrapped)conflict=JSON.parse(conflict);assert.equal(conflict.route_schema_version,1);const payload=conflict.event.payload;payload.change.display_name='Conflicting synthetic name';conflict.event.payload=payload;const conflictBytes=JSON.stringify(stringWrapped?JSON.stringify(conflict):conflict);
 await producer.send({topic:binding.topic,messages:[{value:tapMessages[0]},{value:conflictBytes},{value:JSON.stringify({secret:'SYNTHETIC_PRIVATE_NEVER_DIAGNOSTICS'})}]});
 stage='replay_and_quarantine';await until(async()=>{const offsets=await admin.fetchOffsets({groupId:group,topics:[binding.topic]});return offsets[0]?.partitions[0]?.offset==='7';},'committed_offsets');
 assert.equal(await count(),4,'replay and conflicts cannot add business rows');
 const checkpoint=JSON.parse(await http.query('SELECT state,toString(first_quarantine_offset) AS first FROM organization.control_capture_checkpoint FINAL WHERE topic={topic:String} FORMAT JSONEachRow',{topic:binding.topic}));assert.equal(checkpoint.state,'QUARANTINED');assert.equal(checkpoint.first,'5');
 assert(!workerOutput.includes('SYNTHETIC_PRIVATE_NEVER_DIAGNOSTICS'));assert(!workerOutput.includes('Conflicting synthetic name'));
 assert.equal((await db.query('SELECT count(*)::int AS n FROM kabipay_ops.organization_outbox')).rows[0].n,4);
 const reader=new ControlHttp({url:'http://127.0.0.1:18123',user:chReader,password:'synthetic-control-reader-only'});
 assert.equal(Number(JSON.parse(await reader.query('SELECT count() AS n FROM organization.control_history FINAL WHERE metadata_source={source:String} FORMAT JSONEachRow',{source:binding.connector_name})).n),3,'app row policy hides NULL-organization identity');
 await assert.rejects(reader.query('SELECT count() FROM organization.security_audit'));
 await assert.rejects(reader.query('SELECT count() FROM organization.business_history'));
 console.log(JSON.stringify({result:'PASS',actual_0113_and_0114_sequence:true,actual_store_sql:true,real_debezium_events:4,principal_org_null_retained:true,proposed_installed_sequence:[proposed.sequence,installed.sequence],configuration_revision:[proposed.revision,installed.revision],rollback_not_captured:true,failed_sink_held_offset:'3',sink_recovery_replayed:true,replay_dedup:true,conflicts_quarantined:true,broker_offset:'7',checkpoint:'QUARANTINED',cdc_private_read_and_write_denied:true,restricted_ch_worker:true,reader_security_and_company_history_denied:true,reader_null_org_hidden:true}));
}catch(error){
 const workerCodes=workerOutput.split('\n').flatMap(line=>{try{const value=JSON.parse(line);return typeof value.code==='string'&&/^[A-Z_]+$/.test(value.code)?[value.code]:[];}catch{return [];}}).slice(-10);
 console.error(JSON.stringify({stage,error_code:error.code??error.name,worker_codes:workerCodes,received_cdc:tapMessages.length}));throw error;
}finally{
 if(worker&&worker.exitCode===null){worker.kill('SIGTERM');await new Promise(r=>worker.once('exit',r));}
 await new Promise(r=>proxy.close(r));
 await tap.disconnect().catch(()=>{});await producer.disconnect().catch(()=>{});await admin.disconnect().catch(()=>{});
 await http.query(`DROP ROW POLICY IF EXISTS ${chPolicy} ON organization.control_history`).catch(()=>{});await http.query(`DROP USER IF EXISTS ${chReader}`).catch(()=>{});await http.query(`DROP USER IF EXISTS ${chWorker}`).catch(()=>{});
 await fetch(`http://127.0.0.1:18083/connectors/${connector}`,{method:'DELETE',signal:AbortSignal.timeout(10000)}).catch(()=>{});
 await db.end().catch(()=>{});
 if(created){await adminDb.query('SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname=$1',[database]);for(let attempt=0;attempt<20;attempt++){try{await adminDb.query('SELECT pg_drop_replication_slot($1) WHERE EXISTS(SELECT 1 FROM pg_replication_slots WHERE slot_name=$1 AND database=$2 AND NOT active)',[slot,database]);const active=await adminDb.query('SELECT 1 FROM pg_replication_slots WHERE slot_name=$1',[slot]);if(!active.rowCount)break;}catch{}await new Promise(r=>setTimeout(r,250));}await adminDb.query(`DROP DATABASE ${database} WITH (FORCE)`);await adminDb.query(`DROP ROLE IF EXISTS ${role}`);}
 await adminDb.end().catch(()=>{});assert(directory.startsWith(join(tmpdir(),'kabipay-control-')));rmSync(directory,{recursive:true,force:true});
}
