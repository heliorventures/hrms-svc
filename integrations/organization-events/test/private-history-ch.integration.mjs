// Opt-in actual ClickHouse fixture; every row uses a fresh synthetic scope.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {randomUUID} from 'node:crypto';
import {PrivateClickHouse} from '../src/private/clickhouse.mjs';
import {PrivateHistoryQuery,encodeProof,decodeProof} from '../src/private/query.mjs';
import {sourceHash} from '../src/private/contract.mjs';
const file=process.env.PRIVATE_CLICKHOUSE_TEST_BINDING_FILE;
if(!file)throw Error('explicit isolated ClickHouse fixture binding required');
const fixture=JSON.parse(readFileSync(file));if(fixture.fixture!=='private-pipeline-20261019'||!/^http:\/\/127\.0\.0\.1:\d+\/$/.test(fixture.url))throw Error('fixture boundary');
const writer=new PrivateClickHouse({url:fixture.url,user:'private_audit_worker',password:'private_fixture',purpose:'audit',isolated:true});
const reader=new PrivateClickHouse({url:fixture.url,user:'private_audit_reader',password:'private_fixture',purpose:'audit',isolated:true});
const tenant=randomUUID(),manifest=randomUUID(),key=Buffer.alloc(32,29),at='2026-10-19T01:02:03.123456Z',now=Math.floor(Date.now()/1000),keepAlive=setInterval(()=>{},1000);
try{
 for(const database of ['fixture_main','fixture_other']){
  const id=randomUUID(),raw=`{"id":"${id}","tenant_id":"${tenant}","user_id":null,"entity_type":"FUTURE","entity_id":null,"action":"UNKNOWN","before_state":{"amount":9007199254740993},"after_state":null,"ip_address":null,"user_agent":null,"created_at":"${at}"}`;
  await writer.writeRecord({scope_id:tenant,tenant_id:tenant,record_id:id,source_database:database,source_schema:'source_one',source_table:'audit_log',source_generation:1,source_position:1,source_kind:'legacy_audit_log',resource_type:'audit_record',resource_id:id,occurred_at:at.replace('T',' ').replace('Z',''),source_sha256:sourceHash(Buffer.from(raw)),source_size:Buffer.byteLength(raw),raw_source:raw,actor_id:null,action:'UNKNOWN',entity_type:'FUTURE'});
 }
 const coverageHash='a'.repeat(64);
 const exactPosition='9007199254740993';
 await writer.checkpoint({scope_id:tenant,source_generation:1,topic:`organization.audit.${tenant}`,partition_id:0,kafka_offset:exactPosition,record_id:manifest,source_sha256:coverageHash});
 const positions=await writer.rows(`SELECT kafka_offset,toUInt64({position:String}) AS source_position FROM ${writer.table('checkpoint')} FINAL WHERE scope_id={scope:UUID}`,{scope:tenant,position:exactPosition});
 assert.equal(positions[0].kafka_offset,exactPosition,'Kafka offsets must retain all 64-bit decimal digits');
 assert.equal(positions[0].source_position,exactPosition,'Source positions must retain all 64-bit decimal digits');
 await writer.insert('coverage',{scope_id:tenant,manifest_id:manifest,source_generation:1,source_database:'fixture_main',source_schema:'source_one',source_table:'audit_log',record_count:1,source_sha256:coverageHash,restored_sha256:coverageHash,acceptance_sha256:'b'.repeat(64),coverage_kind:'INITIAL_ARCHIVE',accepted_at:new Date().toISOString().replace('T',' ').replace('Z',''),expires_at:null});
 // Current source generation2 still reads its accepted generation1 archive.
 const service=new PrivateHistoryQuery({bindings:[{tenant_id:tenant,source_generation:2,database:'fixture_main',schema:'source_one',key_b64:key.toString('base64')}],store:reader});
 const filter={tenant_id:tenant,entity_type:null,entity_id:null,actions:[],since:null,until:null,offset:0,limit:200};
 const request={version:1,tenant_id:tenant,source_generation:2,request_id:randomUUID(),issued_at:now,filter,coverage_id:manifest,coverage_sha256:coverageHash};
 const response=decodeProof(await service.execute(encodeProof(request,key,'request')),key,'response');
 assert.equal(response.page.records.length,1,'RSEC-2 query must exclude the uncovered physical source');
 assert.equal(response.page.total,1,'RSEC-2 count must use the same source predicates');
 assert(response.page.records[0].includes('9007199254740993'),'unknown numeric JSON must retain exact digits');
 await assert.rejects(()=>service.execute(encodeProof({...request,source_generation:1,request_id:randomUUID()},key,'request')),/DENIED/);
 console.log('PASS actual ClickHouse: source-scoped audit rows/count, raw numeric wire, initial-archive lineage, stale source proof rejection');
}finally{clearInterval(keepAlive);}
