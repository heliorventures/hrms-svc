import test from 'node:test';
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import {PrivateHistoryQuery,encodeProof,decodeProof} from '../src/private/query.mjs';
const tenant='11111111-1111-4111-8111-111111111111',key=Buffer.alloc(32,23),now=1792368000;
const coverage=randomUUID(),hash='a'.repeat(64),filter={tenant_id:tenant,entity_type:null,entity_id:null,actions:[],since:null,until:null,offset:0,limit:200};
const request=()=>({version:1,tenant_id:tenant,source_generation:1,request_id:randomUUID(),issued_at:now,filter,coverage_id:coverage,coverage_sha256:hash});
function setup(covered=true){const calls=[];return {calls,service:new PrivateHistoryQuery({bindings:[{tenant_id:tenant,source_generation:1,key_b64:key.toString('base64')}],store:{async requireCoverage(r){if(!covered)throw Error('MISSING_COVERAGE');assert.equal(r.coverage_id,coverage);},async readAudit(f){calls.push(f);return {records:[],total:0};}},now:()=>now})};}
test('only verified tenant-bound source can query covered full history and authenticate response',async()=>{
 const {service,calls}=setup(),r=request(),proof=encodeProof(r,key,'request'),result=await service.execute(proof);
 const response=decodeProof(result,key,'response');assert.equal(response.request_id,r.request_id);assert.equal(response.coverage_id,coverage);assert.deepEqual(response.page,{records:[],total:0});assert.equal(calls.length,1);
 await assert.rejects(()=>service.execute(proof),/DENIED/);assert.equal(calls.length,1);
});
test('forged, expired, cross-tenant, extra fields, SQL and invalid limits are rejected before storage',async()=>{
 for(const change of [r=>r.issued_at-=61,r=>r.issued_at+=6,r=>r.filter={...r.filter,tenant_id:randomUUID()},r=>r.sql='SELECT secret',r=>r.filter={...r.filter,limit:501},r=>r.source_generation=2]){
  const {service,calls}=setup(),r=request();change(r);await assert.rejects(()=>service.execute(encodeProof(r,key,'request')));assert.equal(calls.length,0);
 }
 const {service,calls}=setup();await assert.rejects(()=>service.execute(encodeProof(request(),Buffer.alloc(32,24),'request')));assert.equal(calls.length,0);
});
test('absence of accepted legacy coverage is an error rather than empty history',async()=>{
 const {service,calls}=setup(false);await assert.rejects(()=>service.execute(encodeProof(request(),key,'request')),/COVERAGE/);assert.equal(calls.length,0);
});
test('raw audit JSON crosses the signed API without rounding large numbers or rewriting fields',async()=>{
 const raw=`{"id":"${randomUUID()}","tenant_id":"${tenant}","before_state":{"amount":9007199254740993,"decimal":1.234567890123456789},"unknown":null}`;
 const service=new PrivateHistoryQuery({bindings:[{tenant_id:tenant,source_generation:1,key_b64:key.toString('base64')}],store:{async requireCoverage(){},async readAudit(){return {records:[raw],total:1};}},now:()=>now});
 const result=await service.execute(encodeProof(request(),key,'request'));
 assert.equal(decodeProof(result,key,'response').page.records[0],raw);
});
