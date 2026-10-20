import {createHash} from 'node:crypto';
import {uuid} from './contract.mjs';
import {encodeProof,decodeProof} from './query.mjs';
const denied=()=>{throw Error('PRIVATE_HISTORY_PROOF_DENIED');};
const exact=(o,keys)=>{if(!o||Array.isArray(o)||typeof o!=='object'||Object.keys(o).length!==keys.length||keys.some(k=>!Object.hasOwn(o,k)))denied();};
export const encodeDeliveryProof=(v,k,d)=>encodeProof(v,k,d,'delivery-history');
export const decodeDeliveryProof=(v,k,d)=>decodeProof(v,k,d,'delivery-history');
const tables=['outbox_event','webhook_delivery_log','employee_finance_outbox'];
const fields='record_id,resource_id,source_kind,occurred_at,source_sha256,raw_source';
export class DeliveryHistoryStore {
 constructor(db){if(db.purpose!=='delivery')denied();this.db=db;}
 async requireCoverage(r){
  const c=r.coverage,rows=await this.db.rows(`SELECT manifest_id FROM ${this.db.table('coverage')} FINAL WHERE scope_id={tenant:UUID} AND manifest_id={id:UUID} AND source_generation>0 AND source_generation<={generation:UInt64} AND source_database={database:String} AND source_schema={schema:String} AND source_table={table:String} AND source_sha256={hash:String} AND restored_sha256={hash:String} AND coverage_kind='INITIAL_ARCHIVE' AND isNull(expires_at) AND revoked=0 AND accepted_at<=now64(6) LIMIT 2`,{tenant:r.tenant_id,id:c.id,generation:r.source_generation,database:r.source_database,schema:r.source_schema,table:c.table,hash:c.sha256},2048);
  if(rows.length!==1)throw Error('PRIVATE_HISTORY_COVERAGE_REQUIRED');
 }
 async read(f){
  if(!['source_database','source_schema'].every(k=>typeof f[k]==='string'&&/^[A-Za-z0-9_][A-Za-z0-9_-]{0,127}$/.test(f[k])))denied();
  const table=this.db.table('record'),scope='scope_id={tenant:UUID} AND tenant_id={tenant:UUID} AND source_database={database:String} AND source_schema={schema:String}',params={tenant:f.tenant_id,database:f.source_database,schema:f.source_schema,limit:f.limit,aggregates:JSON.stringify(f.aggregates),ids:JSON.stringify(f.event_ids)};
  let next_cursor=null,candidateIds=[];
  let sql;
  if(f.mode==='logs')sql=`SELECT ${fields},JSONExtractString(raw_source,'event_name') AS event_name FROM ${table} FINAL WHERE ${scope} AND (source_kind='legacy_webhook_delivery_log' OR (source_kind='delivery_outcome' AND resource_id IN (SELECT record_id FROM ${table} FINAL WHERE ${scope} AND source_kind='webhook'))) ORDER BY occurred_at DESC,record_id DESC LIMIT {limit:UInt64}`;
  else {
   // Group before limiting so a large destination fanout cannot hide other events.
   const event="if(source_kind='webhook',toString(resource_id),toString(record_id))";
   const body="if(JSONHas(raw_source,'payload','body'),JSONExtractRaw(raw_source,'payload','body'),JSONExtractRaw(raw_source,'payload'))";
   const aggregate=`if(source_kind='webhook',JSONExtractString(${body},'aggregate_type'),JSONExtractString(raw_source,'aggregate_type'))`;
   const predicate=`${scope} AND source_kind IN ('webhook','legacy_outbox_event') AND has(JSONExtract({aggregates:String},'Array(String)'),${aggregate})`;
   const wanted=f.event_ids.length?` AND has(JSONExtract({ids:String},'Array(String)'),${event})`:'';
   const before=f.before?' HAVING (max(occurred_at),event_id)<(parseDateTime64BestEffort({before_time:String},6,\'UTC\'),{before_id:String})':'';
   if(f.before){params.before_time=f.before.occurred_at;params.before_id=f.before.event_id;}
   const candidates=await this.db.rows(`SELECT ${event} AS event_id,toString(max(occurred_at)) AS last_time FROM ${table} FINAL WHERE ${predicate}${wanted} GROUP BY event_id${before} ORDER BY max(occurred_at) DESC,event_id DESC LIMIT {limit:UInt64}`,params,65536);
   if(!candidates.length)return {records:[],next_cursor:null};
   candidateIds=candidates.map(c=>c.event_id);params.candidates=JSON.stringify(candidateIds);if(candidates.length===f.limit){const last=candidates.at(-1);next_cursor={occurred_at:last.last_time.replace(' ','T')+'Z',event_id:last.event_id};}
   sql=`SELECT ${fields} FROM ${table} FINAL WHERE ${predicate}${wanted} AND has(JSONExtract({candidates:String},'Array(String)'),${event}) ORDER BY occurred_at DESC,record_id DESC LIMIT 129001`;
  }
  const records=await this.db.rows(sql,params,64*1024*1024);if(records.length>f.limit*258)throw Error('PRIVATE_HISTORY_FANOUT_LIMIT');
  if(f.mode==='events'){const seen=new Set(records.map(r=>r.source_kind==='webhook'?r.resource_id:r.record_id));if(candidateIds.some(id=>!seen.has(id)))throw Error('PRIVATE_HISTORY_EVENT_GAP');}
  if(f.mode==='logs'&&records.length){const ids=records.filter(r=>r.source_kind==='delivery_outcome').map(r=>r.resource_id);if(ids.length){const metadata=await this.db.rows(`SELECT record_id,JSONExtractString(raw_source,'event_type') AS event_name FROM ${table} FINAL WHERE ${scope} AND source_kind='webhook' AND has(JSONExtract({ids:String},'Array(String)'),toString(record_id))`,{tenant:f.tenant_id,database:f.source_database,schema:f.source_schema,ids:JSON.stringify(ids)},131072),names=new Map(metadata.map(r=>[r.record_id,r.event_name]));for(const r of records)if(r.source_kind==='delivery_outcome')r.event_name=names.get(r.resource_id)??null;}}
  return {records:records.map(r=>({...r,occurred_at:r.occurred_at.includes('T')?r.occurred_at:r.occurred_at.replace(' ','T')+'Z'})),next_cursor};
 }
}
export class DeliveryHistoryQuery {
 constructor({bindings,store,now=()=>Math.floor(Date.now()/1000)}){
  if(!Array.isArray(bindings)||!bindings.length||bindings.length>256)denied();this.bindings=new Map();this.replay=new Map();Object.assign(this,{store,now});const used=new Set();
  for(const b of bindings){const key=Buffer.from(b.key_b64??'','base64');if(!uuid(b.tenant_id)||!Number.isSafeInteger(b.source_generation)||b.source_generation<1||key.length!==32||key.toString('base64')!==b.key_b64||this.bindings.has(b.tenant_id)||used.has(b.key_b64)||!['database','schema'].every(k=>typeof b[k]==='string'&&/^[A-Za-z0-9_][A-Za-z0-9_-]{0,127}$/.test(b[k])))denied();used.add(b.key_b64);this.bindings.set(b.tenant_id,{...b,key});}
 }
 async execute(proof){
  exact(proof,['payload','signature']);if(typeof proof.payload!=='string'||proof.payload.length>24000)denied();let candidate;try{candidate=JSON.parse(Buffer.from(proof.payload,'base64url'));}catch{denied();}
  const b=this.bindings.get(candidate?.tenant_id);if(!b)denied();const r=decodeDeliveryProof(proof,b.key,'request');exact(r,['version','tenant_id','source_generation','request_id','issued_at','filter','coverages']);
  if(r.version!==1||r.tenant_id!==b.tenant_id||r.source_generation!==b.source_generation||!uuid(r.request_id)||!Number.isSafeInteger(r.issued_at)||r.issued_at<this.now()-60||r.issued_at>this.now()+5)denied();
  exact(r.filter,['mode','aggregates','event_ids','limit','before']);const f=r.filter;
  if(!['events','logs'].includes(f.mode)||!Number.isSafeInteger(f.limit)||f.limit<1||f.limit>500||!Array.isArray(f.aggregates)||f.aggregates.length>16||!f.aggregates.every(s=>typeof s==='string'&&s.length<=128)||!Array.isArray(f.event_ids)||f.event_ids.length>500||!f.event_ids.every(uuid))denied();
  if(f.before!==null){exact(f.before,['occurred_at','event_id']);if(f.mode!=='events'||!uuid(f.before.event_id)||typeof f.before.occurred_at!=='string'||!/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,6})?Z$/.test(f.before.occurred_at)||!Number.isFinite(Date.parse(f.before.occurred_at)))denied();}
  if(!Array.isArray(r.coverages)||r.coverages.length!==3||new Set(r.coverages.map(c=>c.table)).size!==3)denied();for(const c of r.coverages){exact(c,['table','id','sha256']);if(!tables.includes(c.table)||!uuid(c.id)||!/^[a-f0-9]{64}$/.test(c.sha256))denied();}
  const now=this.now();for(const [id,expires] of this.replay)if(expires<now)this.replay.delete(id);const replay=`${r.tenant_id}/${r.request_id}`;if(this.replay.has(replay)||this.replay.size>=10000)denied();this.replay.set(replay,now+66);
  for(const coverage of r.coverages)await this.store.requireCoverage({...r,coverage,source_database:b.database,source_schema:b.schema});
  const page=await this.store.read({...f,tenant_id:r.tenant_id,source_database:b.database,source_schema:b.schema});
  if(!page||!Array.isArray(page.records)||page.records.length>f.limit*258)throw Error('PRIVATE_HISTORY_STORAGE_SCOPE');
  for(const record of page.records){let raw;try{raw=JSON.parse(record.raw_source);}catch{throw Error('PRIVATE_HISTORY_STORAGE_SCOPE');}if(!uuid(record.record_id)||!uuid(record.resource_id)||raw.id!==record.record_id||raw.tenant_id!==r.tenant_id||typeof record.raw_source!=='string'||createHash('sha256').update(record.raw_source).digest('hex')!==record.source_sha256)throw Error('PRIVATE_HISTORY_STORAGE_SCOPE');}
  const response={version:1,tenant_id:r.tenant_id,source_generation:r.source_generation,request_id:r.request_id,issued_at:this.now(),request_hash:createHash('sha256').update(Buffer.from(proof.payload,'base64url')).digest('hex'),page};
  if(Buffer.byteLength(JSON.stringify(response))>64*1024*1024)throw Error('PRIVATE_HISTORY_RESPONSE_LIMIT');return encodeDeliveryProof(response,b.key,'response');
 }
}
export function deliveryHistoryHandler(service){return async(req,res)=>{
 if(req.method!=='POST'||req.url!=='/internal/private-events/delivery-history'){res.writeHead(404);res.end();return;}let size=0;const chunks=[];
 req.on('data',c=>{size+=c.length;if(size>24000){res.writeHead(413);res.end();req.destroy();}else chunks.push(c);});
 req.on('end',async()=>{if(size>24000)return;try{const result=await service.execute(JSON.parse(Buffer.concat(chunks)));res.writeHead(200,{'content-type':'application/json','cache-control':'no-store'});res.end(JSON.stringify(result));}catch(e){const denied=e?.message==='PRIVATE_HISTORY_PROOF_DENIED';res.writeHead(denied?403:503,{'content-type':'application/json','cache-control':'no-store'});res.end(JSON.stringify({error:denied?'ACCESS_DENIED':'HISTORY_UNAVAILABLE'}));}});req.on('error',()=>res.destroy());
};}
