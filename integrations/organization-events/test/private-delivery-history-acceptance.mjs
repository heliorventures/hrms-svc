// Explicit isolated ClickHouse fixture only. Inserts synthetic protected history;
// never publishes executable work or contacts a source/receiver.
import {readFileSync} from 'node:fs';
import {randomUUID,createHash} from 'node:crypto';
import assert from 'node:assert/strict';
import {PrivateClickHouse} from '../src/private/clickhouse.mjs';
import {DeliveryHistoryStore,DeliveryHistoryQuery,encodeDeliveryProof,decodeDeliveryProof} from '../src/private/delivery_history.mjs';
const keepAlive=setInterval(()=>{},1000);
try {
 const file=process.argv[2];if(!file)throw Error('Explicit isolated ClickHouse binding file required');
 const config=JSON.parse(readFileSync(file,'utf8')),url=new URL(config.url);
 if(url.protocol!=='http:'||url.hostname!=='127.0.0.1')throw Error('Only explicit loopback isolated fixture accepted');
 const writer=new PrivateClickHouse({url:url.href,user:'private_delivery_worker',password:'private_fixture',purpose:'delivery',isolated:true});
 const reader=new PrivateClickHouse({url:url.href,user:'private_delivery_reader',password:'private_fixture',purpose:'delivery',isolated:true});
 // On failure retrieve the correlated synthetic server error, without changing
 // the production client's deliberately redacted error vocabulary.
 for(const db of [writer,reader]){const query=db.query.bind(db);db.query=async(sql,params={},body,maxBytes)=>{try{return await query(sql,params,body,maxBytes);}catch(error){if(process.env.PRIVATE_HISTORY_FIXTURE_DEBUG==='1'){const target=new URL(url);target.searchParams.set('query',sql);for(const[k,v]of Object.entries(params))target.searchParams.set('param_'+k,String(v));const response=await fetch(target,{method:'POST',headers:db.headers,body});console.error('ISOLATED_CH_SERVER_ERROR',response.status,(await response.text()).slice(0,2000));}throw error;}};}
 const tenant=randomUUID(),database='delivery_fixture_'+tenant.replaceAll('-','').slice(0,8),schema='Tenant_Delivery',store=new DeliveryHistoryStore(reader);
 const hash=raw=>createHash('sha256').update(raw).digest('hex');
 const stamp=s=>s.replace('T',' ').replace(/Z$/,'');
 async function record(row,kind,resource,time,sourceDatabase=database,sourceSchema=schema){
  const raw=typeof row==='string'?row:JSON.stringify(row),parsed=JSON.parse(raw);
  const entry={scope_id:tenant,tenant_id:tenant,record_id:parsed.id,source_database:sourceDatabase,source_schema:sourceSchema,source_table:kind.startsWith('legacy_')?kind.slice(7):'private_delivery_receipt',source_generation:1,source_position:1,source_kind:kind,resource_type:'synthetic_fixture',resource_id:resource,occurred_at:stamp(time),source_sha256:hash(raw),source_size:Buffer.byteLength(raw),raw_source:raw,actor_id:null,action:'',entity_type:''};
  await writer.writeRecord(entry);return entry;
 }
 const first=randomUUID(),second=randomUUID(),legacy=randomUUID();const intentIds=[];
 for(const [event,time,fanout] of [[first,'2026-10-19T01:00:00.000000Z',3],[second,'2026-10-19T00:00:00.000000Z',1]]){
  for(let i=0;i<fanout;i++){const id=randomUUID();intentIds.push(id);await record({id,tenant_id:tenant,destination_id:randomUUID(),destination_version:1,source_kind:'webhook',resource_id:event,event_type:'expense.approved',payload:{schema_version:1,outbox_id:event,tenant_id:tenant,aggregate_type:'expense',aggregate_id:randomUUID(),event_type:'expense.approved',payload:{amount:'17'},created_at:time},occurred_at:time},'webhook',event,time);}
 }
 const rawLegacy=JSON.stringify({id:legacy,tenant_id:tenant,aggregate_type:'expense',aggregate_id:randomUUID(),event_type:'expense.approved',payload:{amount:'BIG_INTEGER'},status:'PROCESSED',retry_count:0,last_error:null,created_at:'2026-10-18T00:00:00.000000Z',processed_at:'2026-10-18T00:00:01.000000Z',claimed_at:null}).replace('"BIG_INTEGER"','18446744073709551615');
 await record(rawLegacy,'legacy_outbox_event',legacy,'2026-10-18T00:00:00.000000Z');
 await record(rawLegacy,'legacy_outbox_event',legacy,'2026-10-21T00:00:00.000000Z',database+'_other');
 await record(rawLegacy,'legacy_outbox_event',legacy,'2026-10-21T00:00:00.000000Z',database,schema+'_other');
 const finance=randomUUID();await record({id:finance,tenant_id:tenant,destination_id:randomUUID(),destination_version:1,source_kind:'finance_confirmation',resource_id:randomUUID(),event_type:'EXIT_APPROVAL_COMMITTED',payload:{operationId:randomUUID()},occurred_at:'2026-10-19T00:00:00.000000Z'},'finance_confirmation',randomUUID(),'2026-10-19T00:00:00.000000Z');
 for(const intent of [intentIds[0],finance]){const id=randomUUID();await record({id,tenant_id:tenant,intent_id:intent,destination_id:randomUUID(),lease_generation:1,response:{status:'DELIVERED',outcome_code:'http_2xx'},created_at:'2026-10-19T02:00:00.000000Z'},'delivery_outcome',intent,'2026-10-19T02:00:00.000000Z');}
 const log=randomUUID();await record({id:log,tenant_id:tenant,webhook_subscription_id:randomUUID(),event_name:'expense.approved',payload_json:{retained:'legacy'},http_status:204,response_body:'legacy response retained',is_success:true,attempt_number:1,delivered_at:'2026-10-18T00:00:02.000000Z',created_at:'2026-10-18T00:00:02.000000Z'},'legacy_webhook_delivery_log',log,'2026-10-18T00:00:02.000000Z');
 const filter={tenant_id:tenant,source_database:database,source_schema:schema,mode:'events',aggregates:['expense'],event_ids:[],before:null,limit:2};
 const page=await store.read(filter);assert.equal(page.records.length,4);assert.equal(new Set(page.records.map(r=>r.resource_id)).size,2);assert.equal(page.next_cursor.event_id,second);
 const older=await store.read({...filter,before:page.next_cursor});assert.equal(older.records.length,1);assert.equal(older.records[0].raw_source,rawLegacy);assert.equal(older.next_cursor,null);
 const selected=await store.read({...filter,event_ids:[first],limit:1});assert.equal(selected.records.length,3);
 const logs=await store.read({...filter,mode:'logs',limit:10});assert.equal(logs.records.length,2);assert.ok(logs.records.every(r=>r.source_kind==='legacy_webhook_delivery_log'||r.resource_id===intentIds[0]));assert.equal(logs.records.find(r=>r.source_kind==='delivery_outcome').event_name,'expense.approved');
 const coverages=[];for(const table of ['outbox_event','webhook_delivery_log','employee_finance_outbox']){const id=randomUUID();coverages.push({table,id,sha256:'a'.repeat(64)});await writer.insert('coverage',{scope_id:tenant,manifest_id:id,source_generation:1,source_database:database,source_schema:schema,source_table:table,coverage_kind:'INITIAL_ARCHIVE',source_marker_id:null,source_cutoff_lsn:'',through_position:0,expected_partitions:1,covered_partitions:1,record_count:0,source_sha256:'a'.repeat(64),restored_sha256:'a'.repeat(64),acceptance_sha256:'b'.repeat(64),accepted_at:stamp(new Date(Date.now()-60000).toISOString()),expires_at:null,revoked:0});}
 const key=Buffer.alloc(32,11),service=new DeliveryHistoryQuery({bindings:[{tenant_id:tenant,database,schema,source_generation:2,key_b64:key.toString('base64')}],store});
 const request={version:1,tenant_id:tenant,source_generation:2,request_id:randomUUID(),issued_at:Math.floor(Date.now()/1000),filter:{mode:'events',aggregates:['expense'],event_ids:[legacy],before:null,limit:1},coverages};
 const response=decodeDeliveryProof(await service.execute(encodeDeliveryProof(request,key,'request')),key,'response');assert.equal(response.page.records[0].raw_source,rawLegacy);
 await assert.rejects(service.execute(encodeDeliveryProof({...request,source_generation:1,request_id:randomUUID()},key,'request')));
 await assert.rejects(store.requireCoverage({tenant_id:tenant,source_generation:2,source_database:database+'_other',source_schema:schema,coverage:coverages[0]}));
 await assert.rejects(reader.writeRecord({...page.records[0],scope_id:tenant}));
 console.log('PASS: actual isolated ClickHouse delivery grouping/fanout/cursor, original-ID filtering, webhook-only logs, raw-u64 transport, fixed source scope, historical coverage after generation rollover, stale proof rejection and read-only role.');
}finally{clearInterval(keepAlive);}
