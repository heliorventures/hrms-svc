import test from 'node:test';
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import {parseKeyring,encryptFrames} from '../src/private/crypto.mjs';
import {PrivateHistoryProcessor} from '../src/private/history.mjs';
const tenant='11111111-1111-4111-8111-111111111111';
const keys=parseKeyring(JSON.stringify({schema_version:1,keys:[{tenant_id:tenant,purpose:'audit',key_id:'test',key_b64:Buffer.alloc(32,7).toString('base64'),active:true}]}));
const binding={tenant_id:tenant,purpose:'audit',source_generation:1,database:'fixture',schema:'fixture_company',topic:`organization.audit.${tenant}`};
class MemoryHistory {
 chunks=new Map();records=new Map();positions=[];
 chunkKey(h){return [h.source_kind,h.resource_type,h.source_generation,h.record_id,h.frame_index].join('/');}
 async chunk(h){return this.chunks.get(this.chunkKey(h));}
 async writeChunk(c){this.chunks.set(this.chunkKey(c),c);}
 async frames(h){return [...this.chunks.values()].filter(c=>['record_id','source_kind','resource_type','source_generation'].every(k=>c[k]===h[k]));}
 async record(h){return this.records.get(`${h.source_table}/${h.record_id}`);}
 async writeRecord(r){this.records.set(`${r.source_table}/${r.record_id}`,r);}
 async checkpoint(p){this.positions.push(p);}
}
function source(extra={}){return Buffer.from(JSON.stringify({id:randomUUID(),tenant_id:tenant,user_id:null,entity_type:'FUTURE_ENTITY',entity_id:null,action:'UNKNOWN_ACTION',before_state:{unknown:'é🙂'},after_state:null,ip_address:'192.0.2.1',user_agent:'fixture',created_at:'2026-10-19T01:02:03.123456Z',...extra}));}
function frames(bytes){const row=JSON.parse(bytes);return encryptFrames(bytes,{record_id:row.id,tenant_id:tenant,purpose:'audit',source_generation:1,source_kind:'audit_log',resource_type:'audit_record',resource_id:row.id,occurred_at:row.created_at},keys);}
test('private history retains full unknown audit bytes, microseconds and duplicate identity',async()=>{
 const store=new MemoryHistory(),p=new PrivateHistoryProcessor({store,keys,binding}),bytes=source(),f=frames(bytes);
 const a=await p.accept({raw:Buffer.from(JSON.stringify(f[0])),topic:binding.topic,partition:0,offset:'0',sourcePosition:'1'});
 assert.equal(a.complete,true);assert.equal(store.records.get(`audit_log/${f[0].record_id}`).raw_source,bytes.toString());
 assert.equal(store.records.get(`audit_log/${f[0].record_id}`).occurred_at,'2026-10-19 01:02:03.123456');
 await p.accept({raw:Buffer.from(JSON.stringify(f[0])),topic:binding.topic,partition:0,offset:'0',sourcePosition:'1'});
 assert.equal(store.records.size,1);
});
test('durable chunks survive restart and preserve UTF8 split across framing',async()=>{
 const store=new MemoryHistory(),bytes=source({large:'a'.repeat(262000)+'🙂'.repeat(200)}),f=frames(bytes);
 assert.equal(f.length,2);
 let p=new PrivateHistoryProcessor({store,keys,binding});
 assert.equal((await p.accept({raw:Buffer.from(JSON.stringify(f[1])),topic:binding.topic,partition:0,offset:'0',sourcePosition:'1'})).complete,false);
 assert.equal(store.records.size,0);
 p=new PrivateHistoryProcessor({store,keys,binding});
 assert.equal((await p.accept({raw:Buffer.from(JSON.stringify(f[0])),topic:binding.topic,partition:0,offset:'1',sourcePosition:'2'})).complete,true);
 assert.equal(store.records.get(`audit_log/${f[0].record_id}`).raw_source,bytes.toString());
});
test('cross company, purpose, future generation, conflicts and tampering never advance checkpoint',async()=>{
 for(const mutate of [h=>h.tenant_id=randomUUID(),h=>h.purpose='delivery',h=>h.source_generation=2,h=>h.action='extra',h=>h.ciphertext=h.ciphertext.slice(4)+'AAAA']){
  const store=new MemoryHistory(),p=new PrivateHistoryProcessor({store,keys,binding}),f=frames(source());mutate(f[0]);
  await assert.rejects(()=>p.accept({raw:Buffer.from(JSON.stringify(f[0])),topic:binding.topic,partition:0,offset:'0',sourcePosition:'1'}));assert.equal(store.positions.length,0);assert.equal(store.records.size,0);
 }
 const store=new MemoryHistory(),p=new PrivateHistoryProcessor({store,keys,binding}),bytes=source(),f=frames(bytes);
 await p.accept({raw:Buffer.from(JSON.stringify(f[0])),topic:binding.topic,partition:0,offset:'0',sourcePosition:'1'});
 const other=frames(source({...JSON.parse(bytes),action:'CONFLICT'}));
 await assert.rejects(()=>p.accept({raw:Buffer.from(JSON.stringify(other[0])),topic:binding.topic,partition:0,offset:'1',sourcePosition:'2'}),/CONFLICT/);
 assert.equal(store.positions.length,1);
});
test('delivery-looking historical rows are retained as history and never executable',async()=>{
 const store=new MemoryHistory(),p=new PrivateHistoryProcessor({store,keys,binding}),bytes=source({payload:{source_kind:'finance_confirmation'}}),f=frames(bytes);
 const result=await p.accept({raw:Buffer.from(JSON.stringify(f[0])),topic:binding.topic,partition:0,offset:'0',sourcePosition:'1'});
 assert.equal(result.delivery,null);
});
test('original UUID collisions across legacy tables remain separate historical records',async()=>{
 const deliveryKeys=parseKeyring(JSON.stringify({schema_version:1,keys:[{tenant_id:tenant,purpose:'delivery',key_id:'test',key_b64:Buffer.alloc(32,7).toString('base64'),active:true}]}));
 const b={...binding,purpose:'delivery',topic:`organization.delivery.${tenant}`},store=new MemoryHistory(),p=new PrivateHistoryProcessor({store,keys:deliveryKeys,binding:b}),id=randomUUID();
 for(const table of ['outbox_event','webhook_delivery_log']){
  const bytes=source({id}),frames=encryptFrames(bytes,{record_id:id,tenant_id:tenant,purpose:'delivery',source_generation:1,source_kind:`legacy_${table}`,resource_type:table,resource_id:id,occurred_at:'2026-10-19T01:02:03.123456Z'},deliveryKeys);
  const result=await p.accept({raw:Buffer.from(JSON.stringify(frames[0])),topic:b.topic,partition:0,offset:table==='outbox_event'?'0':'1',sourcePosition:table==='outbox_event'?'1':'2'});
  assert.equal(result.delivery,null);
 }
 assert.equal(store.records.size,2);
});
test('capture pulse is typed internal history with exact 64-bit position and no executable work',async()=>{
 const id=randomUUID(),row={id,tenant_id:tenant,source_generation:1,purpose:'audit',through_position:'9007199254740993',source_lsn:'A/FF',created_at:'2026-10-19T01:02:03.123456Z'};
 const store=new MemoryHistory(),p=new PrivateHistoryProcessor({store,keys,binding});
 const f=encryptFrames(Buffer.from(JSON.stringify(row)),{record_id:id,tenant_id:tenant,purpose:'audit',source_generation:1,source_kind:'capture_marker',resource_type:'private_capture_marker',resource_id:id,occurred_at:row.created_at},keys);
 const result=await p.accept({raw:Buffer.from(JSON.stringify(f[0])),topic:binding.topic,partition:0,offset:'0',sourcePosition:'9007199254740994'});
 assert.equal(result.delivery,null);assert(store.records.has(`private_capture_marker/${id}`));
 assert(store.records.get(`private_capture_marker/${id}`).raw_source.includes('9007199254740993'));
});
test('authorized replay emits only original intent references and bounded work options',async()=>{
 const k=parseKeyring(JSON.stringify({schema_version:1,keys:[{tenant_id:tenant,purpose:'delivery',key_id:'test',key_b64:Buffer.alloc(32,7).toString('base64'),active:true}]}));
 const id=randomUUID(),intent=randomUUID(),destination=randomUUID(),at='2026-10-19T01:02:03.123456Z',b={...binding,purpose:'delivery',topic:`organization.delivery.${tenant}`};
 const row={id,tenant_id:tenant,destination_id:destination,destination_version:1,source_kind:'delivery_replay',resource_id:intent,event_type:'private.delivery.replay.v1',payload:{intent_id:intent,intent_hash:'a'.repeat(64),destination_id:destination,destination_version:1,source_generation:1,base_lease:7,retry_budget:3},occurred_at:at};
 const f=encryptFrames(Buffer.from(JSON.stringify(row)),{record_id:id,tenant_id:tenant,purpose:'delivery',source_generation:1,source_kind:'delivery_replay',resource_type:'delivery_replay',resource_id:intent,occurred_at:at},k),store=new MemoryHistory(),p=new PrivateHistoryProcessor({store,keys:k,binding:b});
 const result=await p.accept({raw:Buffer.from(JSON.stringify(f[0])),topic:b.topic,partition:0,offset:'0',sourcePosition:'1'});
 assert.equal(result.delivery.intent_id,intent);assert.equal(result.delivery.intent_hash,'a'.repeat(64));assert.deepEqual(result.workOptions,{baseLease:7,retryBudget:3});
});
test('purpose-specific datasets reject a Finance intent encrypted into the audit stream',async()=>{
 const bytes=source(),id=JSON.parse(bytes).id,f=encryptFrames(bytes,{record_id:id,tenant_id:tenant,purpose:'audit',source_generation:1,source_kind:'finance_confirmation',resource_type:'delivery_intent',resource_id:id,occurred_at:'2026-10-19T01:02:03.123456Z'},keys),store=new MemoryHistory();
 await assert.rejects(()=>new PrivateHistoryProcessor({store,keys,binding}).accept({raw:Buffer.from(JSON.stringify(f[0])),topic:binding.topic,partition:0,offset:'0',sourcePosition:'1'}));
 assert.equal(store.positions.length,0);
});
