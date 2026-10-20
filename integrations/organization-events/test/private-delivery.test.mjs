import test from 'node:test';
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import {readFileSync} from 'node:fs';
import {DeliveryCoordinator,newWork,signRequest,verifyResponse,signResponse} from '../src/private/delivery.mjs';
const now=1792368000,key=Buffer.alloc(32,7),ref={tenant_id:'11111111-1111-4111-8111-111111111111',intent_id:randomUUID(),destination_id:randomUUID(),source_generation:1,destination_version:1,intent_hash:'a'.repeat(64)};
test('source Rust frozen proof vector matches the Node wire',()=>{
 const vector=JSON.parse(readFileSync(new URL('./private-delivery-vector.json',import.meta.url))),k=Buffer.from(vector.key_b64,'base64');
 const signed=signRequest(JSON.parse(vector.request_json),k);
 assert.deepEqual(signed,vector.request);assert.deepEqual(signResponse(JSON.parse(vector.response_json),k),vector.response);
 assert.equal(verifyResponse(vector.response,signed.request,k,100).status,'DELIVERED');
});
function response(request,status,lease,code='fixture'){return {...request,lease_generation:lease,status,outcome_code:code,acknowledgement_id:status==='DELIVERED'?randomUUID():null};}
test('delivery wire authenticates full source response and rejects identity/time/domain tamper',()=>{
 const r={version:1,...ref,lease_generation:1,request_id:randomUUID(),issued_at:now},proof=signRequest(r,key),signed=signResponse(response(r,'DELIVERED',1),key);
 assert.equal(typeof proof.signature,'string');assert.equal(verifyResponse(signed,r,key,now).status,'DELIVERED');
 for(const field of ['intent_id','tenant_id','intent_hash','request_id','source_generation']){const bad=structuredClone(signed);bad.response[field]=field.endsWith('_id')?randomUUID():'wrong';assert.throws(()=>verifyResponse(bad,r,key,now));}
 assert.throws(()=>verifyResponse(signed,r,Buffer.alloc(32,8),now));assert.throws(()=>verifyResponse(signed,r,key,now+61));
});
test('per destination success is never resent after lost Kafka acknowledgement',async()=>{
 let sends=0,delivered=false;const client={async exchange(r){if(r.lease_generation===0)return response(r,delivered?'DELIVERED':'PENDING',delivered?1:0);sends++;delivered=true;return response(r,'DELIVERED',1);}};
 const worker=new DeliveryCoordinator({client,now:()=>now}),item=newWork(ref,now);
 assert.equal((await worker.run(item)).kind,'complete');assert.equal((await worker.run(item)).kind,'complete');assert.equal(sends,1);
});
test('safe failures use bounded broker retry and stale original work cannot advance new leases',async()=>{
 let current=0,sends=0;const client={async exchange(r){if(r.lease_generation===0)return response(r,current?'RETRYABLE':'PENDING',current);sends++;current=r.lease_generation;return response(r,'RETRYABLE',current);}};
 const worker=new DeliveryCoordinator({client,now:()=>now,delays:[1,2,3]}),item=newWork(ref,now),first=await worker.run(item);
 assert.equal(first.kind,'retry');assert.equal(first.next.lease_generation,2);
 assert.equal((await worker.run({...first.next,available_at:now})).kind,'retry');
 assert.equal((await worker.run(item)).kind,'stale');assert.equal(sends,2);
 let next={...first.next,available_at:now};next.lease_generation=3;next.attempt=2;const third=await worker.run(next);assert.equal(third.kind,'retry');
 assert.equal((await worker.run({...third.next,available_at:now})).kind,'dlq');assert.equal(sends,4);
});
test('transport ack loss probes the same lease and uncertainty cannot become another send',async()=>{
 let sends=0,status='PENDING',lease=0;const client={async exchange(r){if(!r.lease_generation)return response(r,status,lease);sends++;status='UNCERTAIN';lease=1;throw Error('lost response');}};
 const worker=new DeliveryCoordinator({client,now:()=>now}),first=await worker.run(newWork(ref,now));assert.equal(first.kind,'retry');assert.equal(first.next.lease_generation,1);
 assert.equal((await worker.run({...first.next,available_at:now})).kind,'dlq');assert.equal(sends,1);
});
test('future retry is not acknowledged or executed and work scope is validated',async()=>{
 let calls=0;const worker=new DeliveryCoordinator({client:{exchange(){calls++;throw Error();}},now:()=>now});
 assert.equal((await worker.run({...newWork(ref,now),available_at:now+100})).kind,'wait');assert.equal(calls,0);
 await assert.rejects(()=>worker.run({...newWork(ref,now),ref:{...ref,intent_hash:'bad'}}));
});
test('authorized replay starts after the source lease fence with a fresh bounded retry budget',async()=>{
 let lease=7,sends=0;const client={async exchange(r){if(!r.lease_generation)return response(r,'PENDING',lease);sends++;lease=r.lease_generation;return response(r,'DELIVERED',lease);}};
 const worker=new DeliveryCoordinator({client,now:()=>now}),item=newWork(ref,now,{baseLease:7,retryBudget:3});
 assert.equal(item.lease_generation,8);assert.equal(item.base_lease,7);
 assert.equal((await worker.run(item)).kind,'complete');assert.equal(sends,1);
});
