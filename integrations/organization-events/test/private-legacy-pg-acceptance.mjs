// Explicit isolated PostgreSQL fixture. Optional owned ClickHouse binding uses
// the real protected store; this publisher hook does not claim Kafka transport.
import assert from 'node:assert/strict';import {randomUUID} from 'node:crypto';
import {mkdtempSync,readFileSync} from 'node:fs';import {tmpdir} from 'node:os';import {join} from 'node:path';
import pg from 'pg';
import {SourceLegacyApi} from '../src/private/legacy-cli.mjs';
import {ExportFiles,LegacyExporter,LEGACY_TABLES} from '../src/private/legacy-export.mjs';
import {restoreExport} from '../src/private/legacy-reconcile.mjs';
import {parseKeyring} from '../src/private/crypto.mjs';
import {PrivateHistoryProcessor} from '../src/private/history.mjs';
import {PrivateClickHouse} from '../src/private/clickhouse.mjs';
const connectionString=process.env.PRIVATE_LEGACY_FIXTURE_DSN,url=new URL(connectionString);if(!['127.0.0.1','localhost'].includes(url.hostname)||url.pathname!=='/private_retirement_fixture')throw Error('ISOLATED_SOURCE_FIXTURE_REQUIRED');
const tenant=process.env.PRIVATE_LEGACY_FIXTURE_TENANT,ops=process.env.PRIVATE_LEGACY_FIXTURE_OPS_SCOPE,pool=new pg.Pool({connectionString,max:1,ssl:false}),database='private_retirement_fixture',schema='source_one';
const chFile=process.env.PRIVATE_CLICKHOUSE_TEST_BINDING_FILE,ch=chFile?JSON.parse(readFileSync(chFile)):null;
if(ch&&(ch.fixture!=='private-pipeline-20261019'||!/^http:\/\/127\.0\.0\.1:\d+\/$/.test(ch.url)))throw Error('ISOLATED_HISTORY_FIXTURE_REQUIRED');
const keepAlive=setInterval(()=>{},1000);
try{
 let total=0,framesTotal=0;
 for(const [scope,sourceSchema,tables] of [[tenant,schema,['audit_log','outbox_event','webhook_delivery_log','employee_finance_outbox']],[ops,'kabipay_ops',['operator_audit_log']]]){
  await pool.query(`INSERT INTO "${sourceSchema}".private_source_cutover(tenant_id,producer_generation) VALUES($1,1) ON CONFLICT(tenant_id) DO NOTHING`,[scope]);
  const state=(await pool.query(`SELECT state FROM "${sourceSchema}".private_source_cutover WHERE tenant_id=$1`,[scope])).rows[0];
  if(state.state==='PREPARED')await pool.query(`SELECT "${sourceSchema}".private_fence_legacy($1,1)`,[scope]);
  for(const table of tables){
   const prior=(await pool.query(`SELECT id,accepted_at,source_sha256,restore_sha256 FROM "${sourceSchema}".private_legacy_export WHERE tenant_id=$1 AND source_table=$2 AND producer_generation=1`,[scope,table])).rows[0];const purpose=LEGACY_TABLES[table],b={tenant_id:scope,source_database:database,source_schema:sourceSchema,source_table:table,source_generation:1,manifest_id:prior?.id??randomUUID()},api=new SourceLegacyApi(pool,b),keys=parseKeyring(JSON.stringify({schema_version:1,keys:[{tenant_id:scope,purpose,key_id:'isolated-v1',key_b64:Buffer.alloc(32,7).toString('base64'),active:true}]})),chunks=new Map(),records=new Map();
   const chunkKey=c=>{const h=c.envelope?JSON.parse(c.envelope):c;return `${h.source_kind}/${h.resource_type}/${h.source_generation}/${c.record_id}/${c.frame_index}`;},recordKey=c=>`${c.source_table}/${c.record_id}`;
   const store=ch?new PrivateClickHouse({url:ch.url,user:`private_${purpose}_worker`,password:'private_fixture',purpose,isolated:true}):{chunk:async c=>chunks.get(chunkKey(c)),writeChunk:async c=>chunks.set(chunkKey(c),c),frames:async c=>[...chunks.values()].filter(x=>x.record_id===c.record_id&&(!c.source_kind||JSON.parse(x.envelope).source_kind===c.source_kind)&&(!c.resource_type||JSON.parse(x.envelope).resource_type===c.resource_type)&&Number(x.source_generation)===Number(c.source_generation)).sort((a,b)=>a.frame_index-b.frame_index),record:async c=>records.get(recordKey(c)),writeRecord:async c=>records.set(recordKey(c),c),checkpoint:async()=>{}};
   const topic=purpose==='operator_audit'?'organization.operator_audit.metadata':`organization.${purpose}.${scope}`,processor=new PrivateHistoryProcessor({store,keys,binding:{tenant_id:scope,source_generation:1,purpose,database,schema:sourceSchema,topic}}),originalPublish=api.publish.bind(api);
   api.publish=async frames=>{if(!prior?.accepted_at)await originalPublish(frames);for(const frame of frames){const sql=sourceSchema==='kabipay_ops'?`SELECT source_position::text AS pos,payload AS envelope FROM kabipay_ops.organization_outbox WHERE id=$1`:`SELECT source_position::text AS pos,event_envelope AS envelope FROM "${sourceSchema}".company_event_outbox WHERE id=$1`;const row=(await pool.query(sql,[frame.event_id])).rows[0];assert.ok(row);assert.deepEqual(row.envelope,frame);const result=await processor.accept({raw:Buffer.from(JSON.stringify(row.envelope)),topic,partition:0,offset:String(framesTotal++),sourcePosition:row.pos});assert.equal(result.delivery,null);}};
   const files=new ExportFiles(mkdtempSync(join(tmpdir(),'pg-raw-export-'))),exporter=new LegacyExporter({source:api,keys,files,binding:b});const result=await exporter.run();assert.equal(result.complete,true);await exporter.run();const proof=await restoreExport({files,store,keys,binding:b,directory:mkdtempSync(join(tmpdir(),'pg-raw-restore-')),acceptedBy:'isolated-fixture'});if(!prior?.accepted_at)await api.accept(proof);else {assert.equal(prior.source_sha256,proof.restore_sha256);assert.equal(prior.restore_sha256,proof.restore_sha256);}assert.equal(proof.restore_sha256,result.source_sha256);total+=proof.row_count;
   await assert.rejects(api.accept({...proof,source_generation:2}));await assert.rejects(api.accept({...proof,restore_sha256:'0'.repeat(64)}));
   console.log(JSON.stringify({table,rows:proof.row_count,source_and_restore_sha256_equal:true,accepted:true}));
  }
 }
 console.log(JSON.stringify({code:'PRIVATE_LEGACY_PG_SOURCE_ACCEPTED',rows:total,published_replayed_frames:framesTotal,transport:ch?'actual-protected-clickhouse-with-direct-source-hook':'in-memory-history-stand-in'}));
}finally{await pool.end();clearInterval(keepAlive);}


