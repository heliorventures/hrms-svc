import test from 'node:test';
import assert from 'node:assert/strict';
import {parseEvent,digest} from '../src/events.mjs';
import {processConsumerRecord} from '../src/consumers.mjs';
const org='00000000-0000-4000-8000-000000000001', tenant='00000000-0000-4000-8000-000000000002', id='00000000-0000-4000-8000-000000000003';
const bindings={company:{organization_id:org,tenant_id:tenant}};
function event(version=1,changes={}) { return {schema_version:1,event_id:'00000000-0000-4000-8000-000000000004',event_type:'timesheet.changed',organization_id:org,source_tenant_id:tenant,aggregate_type:'timesheet_entry',aggregate_id:id,aggregate_version:version,occurred_at:'2026-10-19T00:00:00Z',deleted:false,payload:{employee_id:id,work_date:'2026-10-18',logged_seconds:3600,approved_seconds:0},...changes}; }
test('strict whitelisted contract and fixed topic provenance',()=>{
  assert.equal(parseEvent(Buffer.from(JSON.stringify(event())),'company',bindings).aggregate_version,1);
  for(const e of [event(0),event(1,{source_tenant_id:org}),event(1,{payload:{...event().payload,salary:123}}),event(1,{payload:{...event().payload,logged_seconds:-1}}),event(1,{payload:{...event().payload,employee_id:{toString:'not_callable'}}})]) assert.throws(()=>parseEvent(Buffer.from(JSON.stringify(e)),'company',bindings));
  assert.throws(()=>parseEvent(Buffer.from('{}'),'unknown',bindings));
  assert.equal(parseEvent(Buffer.from(JSON.stringify(event())),'company',{company:{tenant_id:tenant,organization_ids:[org]}}).organization_id,org);
});
test('Debezium unexpanded JSON payload preserves exact heterogeneous event shapes',()=>{
 const raw=Buffer.from(JSON.stringify(JSON.stringify(event())));assert.equal(parseEvent(raw,'company',bindings).event_id,event().event_id);
 assert.throws(()=>parseEvent(Buffer.from(JSON.stringify(JSON.stringify({...event(),payload:{...event().payload,password:'private'}}))),'company',bindings));
 assert.throws(()=>parseEvent(Buffer.from(JSON.stringify(JSON.stringify(JSON.stringify(event())))),'company',bindings));
});
function fixture() {
  const state={current:null,identities:{},applied:[],history:[],watermarks:[],quarantine:[]};
  const store={transaction:async fn=>{
    const saved=structuredClone(state);
    try { return await fn({membershipClassification:async()=>state.membership??'ACTIVE',identity:async(e,role)=>{const key=role+':'+e.aggregate_version;const hash=state.identities[key];if(hash&&hash!==e.payload_hash)return false;state.identities[key]=e.payload_hash;return true;},checkpoint:async(e,m)=>state.watermarks.push(m),lock:async()=>state.current,apply:async e=>{state.current={source_version:e.aggregate_version,payload_hash:e.payload_hash};state.applied.push(e);},watermark:async(e,m)=>state.watermarks.push(m)}); }
    catch(e){Object.assign(state,saved);throw e;}
  },markConsumerPartial:async m=>state.watermarks.push({...m,state:'PARTIAL'})};
  return {state,store,history:{write:async e=>state.history.push(e)},quarantine:async m=>state.quarantine.push(m)};
}
async function send(f,e,offset='0',role='inbox'){return processConsumerRecord({...f,role,raw:Buffer.from(JSON.stringify(e)),topic:'company',partition:0,offset,bindings});}
test('replays and late versions do not replace latest; correction and tombstone advance',async()=>{
  const f=fixture();await send(f,event());await send(f,event());await send(f,event(2,{payload:{...event().payload,logged_seconds:1800}}));await send(f,event());await send(f,event(3,{deleted:true}));
  assert.deepEqual(f.state.applied.map(e=>e.aggregate_version),[1,2,3]);assert.equal(f.state.applied.at(-1).deleted,true);
});
test('same-version conflicts quarantine independently in history',async()=>{
  const f=fixture();await send(f,event(),'0','history');await send(f,event(1,{deleted:true}),'1','history');assert.equal(f.state.history.length,1);assert.equal(f.state.watermarks.at(-1).state,'QUARANTINED');
});
test('failed history sink leaves its checkpoint untouched while inbox progresses',async()=>{
  const f=fixture(); f.history.write=async()=>{throw Error('offline');};await assert.rejects(send(f,event(),'0','history'));assert.equal(f.state.watermarks.length,0);await send(f,event());assert.equal(f.state.current.source_version,1);
});
test('malformed DLQ contains only hash and position; DLQ failure blocks advancement',async()=>{
  const f=fixture();await send(f,{secret:'private'});assert.equal(f.state.watermarks[0].state,'PARTIAL');assert.equal(f.state.quarantine[0].sha256,digest(Buffer.from(JSON.stringify({secret:'private'}))));assert.equal(JSON.stringify(f.state.quarantine).includes('private'),false);
  const g=fixture();g.quarantine=async()=>{throw Error('offline');};await assert.rejects(send(g,{}));assert.equal(g.state.watermarks.length,0);
});
test('historical accepted membership retains facts but never updates current projection',async()=>{
 const f=fixture();f.state.membership='HISTORICAL';const result=await send(f,event(),'0','history');await send(f,event());assert.equal(result.historical,true);assert.equal(f.state.history.length,1);assert.equal(f.state.applied.length,0);assert.equal(f.state.quarantine.length,0);assert.equal(f.state.watermarks[0].state,'READY');assert.equal(f.state.watermarks[0].code,null);
 const broken=fixture();broken.state.membership='HISTORICAL';broken.history.write=async()=>{throw Error('offline');};await assert.rejects(send(broken,event(),'0','history'));assert.equal(broken.state.watermarks.length,0);
});
test('unknown or pending-only company pair is quarantined without any retained fact',async()=>{
 const f=fixture();f.state.membership='NONE';const result=await send(f,event());assert.equal(result.discarded,true);assert.equal(f.state.history.length,0);assert.equal(f.state.applied.length,0);assert.equal(f.state.quarantine[0].code,'MEMBERSHIP_UNRECOGNIZED');assert.equal(f.state.watermarks.length,0);
});
test('directory and approval names preserve 255 Unicode characters',()=>{
 const directory=event(1,{event_type:'directory.changed',aggregate_type:'employee',payload:{employee_id:id,display_name:'अ'.repeat(255),employment_status:'ACTIVE'}});
 assert.equal(parseEvent(Buffer.from(JSON.stringify(directory)),'company',bindings).payload.display_name.length,255);
 assert.throws(()=>parseEvent(Buffer.from(JSON.stringify({...directory,payload:{...directory.payload,display_name:'अ'.repeat(256)}})),'company',bindings));
 const approval=event(1,{event_type:'approval.changed',aggregate_type:'leave_request',payload:{resource_type:'leave_request',resource_id:id,employee_id:id,employee_display_name:'अ'.repeat(201),status:'PENDING',summary:'Leave request'}});
 assert.equal(parseEvent(Buffer.from(JSON.stringify(approval)),'company',bindings).payload.employee_display_name.length,201);
 assert.throws(()=>parseEvent(Buffer.from(JSON.stringify({...approval,payload:{...approval.payload,status:'pending'}})),'company',bindings));
});
test('typed actor provenance is retained and participates in same-version conflicts',async()=>{
 const actor={kind:'organization_principal',principal_id:id,display_name:'अ'.repeat(255),identity_version:1,source_tenant_id:org,source_user_id:id,authority_kind:'organization_grant',authority_id:id,authority_version:2,command_id:id};
 const e=event(1,{actor});assert.equal(parseEvent(Buffer.from(JSON.stringify(e)),'company',bindings).actor.source_tenant_id,org);
 const f=fixture();await send(f,e,'0','history');await send(f,{...e,actor:{...actor,authority_version:3}},'1','history');assert.equal(f.state.history.length,1);assert.equal(f.state.quarantine[0].code,'VERSION_CONFLICT');
 assert.throws(()=>parseEvent(Buffer.from(JSON.stringify({...e,actor:{...actor,jwt:'private'}})),'company',bindings));
 assert.throws(()=>parseEvent(Buffer.from(JSON.stringify(event(1,{actor:{kind:'local_user',local_user_id:id,source_tenant_id:org}}))),'company',bindings));
 assert.equal(parseEvent(Buffer.from(JSON.stringify(event(1,{actor:{kind:'local_user',local_user_id:id,source_tenant_id:tenant}}))),'company',bindings).actor.local_user_id,id);
 assert.equal(parseEvent(Buffer.from(JSON.stringify(event(1,{actor:null}))),'company',bindings).payload_hash,parseEvent(Buffer.from(JSON.stringify(event())),'company',bindings).payload_hash);
});
