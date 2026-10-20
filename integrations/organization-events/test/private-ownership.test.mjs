import test from 'node:test';
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import {ownershipHash,signOwnership,signOwnershipResponse,verifyOwnershipResponse} from '../src/private/ownership.mjs';
const now=1792368000,key=Buffer.alloc(32,7),tenant=randomUUID();
function request(){return {version:1,tenant_id:tenant,intent_id:randomUUID(),destination_id:randomUUID(),source_generation:1,destination_version:1,intent_hash:'a'.repeat(64),broker_owner_id:randomUUID(),topic:`organization.delivery.work.${tenant}`,partition_id:0,kafka_offset:'9223372036854775800',request_id:randomUUID(),issued_at:now};}
test('source ownership binds durable broker coordinates and cannot imply receiver acknowledgement',()=>{
 const r=request(),response={...r,receipt_sha256:ownershipHash(r),accepted_at:now,already_owned:false};
 assert.equal(typeof signOwnership(r,key).signature,'string');assert.equal(verifyOwnershipResponse(signOwnershipResponse(response,key),r,key,now).already_owned,false);
 const changed={...r,kafka_offset:'9223372036854775801'};assert.notEqual(ownershipHash(changed),response.receipt_sha256);
 assert.throws(()=>verifyOwnershipResponse(signOwnershipResponse(response,key),changed,key,now));
 assert.throws(()=>verifyOwnershipResponse(signOwnershipResponse({...response,receipt_sha256:'b'.repeat(64)},key),r,key,now));
});
test('duplicate publication preserves the first authenticated source ownership receipt',()=>{
 const r=request(),prior='b'.repeat(64),response={...r,receipt_sha256:prior,accepted_at:now-300,already_owned:true};
 assert.equal(verifyOwnershipResponse(signOwnershipResponse(response,key),r,key,now).receipt_sha256,prior);
 assert.throws(()=>verifyOwnershipResponse(signOwnershipResponse(response,Buffer.alloc(32,8)),r,key,now));
});
