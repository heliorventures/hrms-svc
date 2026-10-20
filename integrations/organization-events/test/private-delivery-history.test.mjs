import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {DeliveryHistoryQuery,DeliveryHistoryStore,encodeDeliveryProof,decodeDeliveryProof} from '../src/private/delivery_history.mjs';
const tenant='10000000-0000-4000-8000-000000000001',id='20000000-0000-4000-8000-000000000001',key=Buffer.alloc(32,9);
const coverages=['outbox_event','webhook_delivery_log','employee_finance_outbox'].map(table=>({table,id,sha256:'a'.repeat(64)}));
const request=()=>({version:1,tenant_id:tenant,source_generation:1,request_id:id,issued_at:100,filter:{mode:'events',aggregates:['expense'],event_ids:[],limit:2,before:null},coverages:coverages.map(c=>({...c}))});
test('delivery query authenticates purpose, generation, complete coverage and request hash',async()=>{
 let covered=0;const store={requireCoverage:async()=>{covered++;},read:async()=>({records:[]})};
 const service=new DeliveryHistoryQuery({bindings:[{tenant_id:tenant,source_generation:1,key_b64:key.toString('base64'),database:'source',schema:'public'}],store,now:()=>100});
 const proof=encodeDeliveryProof(request(),key,'request');const result=decodeDeliveryProof(await service.execute(proof),key,'response');assert.equal(covered,3);assert.equal(result.tenant_id,tenant);assert.match(result.request_hash,/^[a-f0-9]{64}$/);
 await assert.rejects(service.execute(proof));
 const wrong=request();wrong.source_generation=2;await assert.rejects(service.execute(encodeDeliveryProof(wrong,key,'request')));
 const incomplete=request();incomplete.coverages.pop();await assert.rejects(service.execute(encodeDeliveryProof(incomplete,key,'request')));
 assert.throws(()=>decodeDeliveryProof(proof,key,'response'));
});
test('event candidate limit groups original IDs before selecting fanout; SQL remains parameterized',async()=>{
 const sql=[];const db={purpose:'delivery',table:s=>`private_history.delivery_${s}`,rows:async(q,p)=>{sql.push([q,p]);return q.includes('SELECT manifest_id')?[{manifest_id:id}]:[];}};
 const store=new DeliveryHistoryStore(db);await store.read({...request().filter,tenant_id:tenant,source_database:'source',source_schema:'public'});
 assert.match(sql[0][0],/GROUP BY event_id/);assert.match(sql[0][0],/LIMIT \{limit:UInt64\}/);assert.equal(sql[0][1].tenant,tenant);assert.ok(!sql[0][0].includes(tenant));
 await store.requireCoverage({...request(),coverage:coverages[0],source_database:'source',source_schema:'public'});assert.match(sql.at(-1)[0],/restored_sha256/);assert.ok(!sql.at(-1)[0].includes('expires_at>'));
});
test('log queries select only webhook attempts and expose safe event diagnostics',async()=>{
 let sql;const db={purpose:'delivery',table:s=>`private_history.delivery_${s}`,rows:async(q)=>{sql=q;return [];}};
 await new DeliveryHistoryStore(db).read({...request().filter,tenant_id:tenant,source_database:'source',source_schema:'public',mode:'logs'});
 assert.match(sql,/source_kind='webhook'/);assert.match(sql,/event_name/);assert.match(sql,/legacy_webhook_delivery_log/);
});
test('signed delivery history keeps raw large integers exact and rejects foreign rows',async()=>{
 const raw=`{"id":"${id}","tenant_id":"${tenant}","payload":{"amount":18446744073709551615}}`;
 const record={record_id:id,resource_id:id,source_kind:'legacy_outbox_event',occurred_at:'2026-10-19T00:00:00.000000Z',source_sha256:createHash('sha256').update(raw).digest('hex'),raw_source:raw};
 const make=(row)=>new DeliveryHistoryQuery({bindings:[{tenant_id:tenant,source_generation:2,key_b64:key.toString('base64'),database:'source',schema:'public'}],store:{requireCoverage:async()=>{},read:async()=>({records:[row],next_cursor:null})},now:()=>100});
 const r=request();r.source_generation=2;const reply=decodeDeliveryProof(await make(record).execute(encodeDeliveryProof(r,key,'request')),key,'response');assert.equal(reply.page.records[0].raw_source,raw);
 const bad={...record,raw_source:raw.replace(tenant,id)};await assert.rejects(make(bad).execute(encodeDeliveryProof(r,key,'request')));
});
test('cursor paging binds original event IDs and timestamp instead of row offsets',async()=>{
 const sql=[];let calls=0;const db={purpose:'delivery',table:s=>`private_history.delivery_${s}`,rows:async(q,p)=>{sql.push([q,p]);return ++calls===1?[{event_id:id,last_time:'2026-10-19 00:00:00.000000'}]:[{record_id:id,source_kind:'legacy_outbox_event',occurred_at:'2026-10-19 00:00:00.000000'}];}};
 const before={event_id:tenant,occurred_at:'2026-10-20T00:00:00.000000Z'};const result=await new DeliveryHistoryStore(db).read({...request().filter,tenant_id:tenant,source_database:'source',source_schema:'public',limit:1,before});
 assert.deepEqual(result.next_cursor,{event_id:id,occurred_at:'2026-10-19T00:00:00.000000Z'});assert.match(sql[0][0],/HAVING/);assert.equal(sql[0][1].before_id,tenant);assert.match(sql[1][0],/candidates:String/);
});
