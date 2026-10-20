import test from 'node:test';
import assert from 'node:assert/strict';
import {parseControlEvent,processControlRecord} from '../src/control-events.mjs';
import {ControlHistory} from '../src/control-history.mjs';
import {envelope,binding,org,tenant,id,principal} from './control-fixture.mjs';
const raw=e=>Buffer.from(JSON.stringify(e));
const parse=e=>parseControlEvent(raw(e),binding.topic,binding);
test('control metadata trust is independent and orgNULL principal history is retained',()=>{
 const parsed=parse(envelope());assert.equal(parsed.organization_id,null);assert.equal(JSON.parse(parsed.actor).principal_id,principal);
 assert.throws(()=>parseControlEvent(raw(envelope()),'company',binding));
 for(const change of [e=>e.source.db='tenant_database',e=>e.source.schema='public',e=>e.source.table='employee',e=>e.source.name='company_connector',e=>e.after.actor_principal_id=id,e=>e.after.payload.actor.source_tenant_id=org,e=>e.after.payload.actor.token='private',e=>e.after.payload.change.password='secret',e=>e.after.aggregate_version=Number.MAX_SAFE_INTEGER+1,e=>e.after.organization_id=org,e=>e.op='u']){const e=envelope();change(e);assert.throws(()=>parse(e));}
 const snapshot=envelope();snapshot.op='r';assert.equal(parse(snapshot).content_hash,parsed.content_hash);
});
test('all current control event contracts preserve purpose-limited changes',()=>{
 const examples={
  'identity.revoked':[null,id,{link_id:id,status:'REVOKED'}],
  'organization.bootstrapped':[org,org,{organization_id:org,name:'Synthetic organization',membership_id:id,role_id:id,binding_id:id,capabilities:['organization:manage'],status:'ACTIVE'}],
  'organization.configuration_changed':[org,id,{request:{operation:'create_role',name:'Reviewers',capabilities:[{code:'history:read',employee_scope:'ALL'}]},id,status:'ACTIVE',version:1,organization_version:2}],
  'relationship.proposed':[org,id,{id,subject:id,manager:principal,graph_version:2}],
  'relationship.manager_changed':[org,id,{id,manager:principal,version:2,graph_version:3}],
  'relationship.ended':[org,id,{id,status:'ENDED',graph_version:3}],
  'relationship.installed':[org,id,{id,membership_id:principal,version:2,graph_version:3}],
  'grant.installed':[org,id,{membership_id:principal,binding_id:id,version:2}],
  'membership.installed':[org,id,{membership_id:id,status:'DETACHED',version:2}],
  'command.cancelled':[org,id,{command_id:id,resource_id:principal,membership_id:tenant,operation:'leave_decision',version:1}],
  'directory.synchronized':[org,id,{membership_id:id,records:200,source_after:null,has_more:false,source_bootstrap:{status:'COMPLETE',phase:5}}],
 };
 for(const [kind,[organization_id,aggregate_id,change]]of Object.entries(examples)){const e=envelope({organization_id,aggregate_id,event_type:kind});e.after.payload.change=change;assert.equal(parse(e).event_type,kind);}
});
test('outbox sequence differs from shared proposed/installed configuration revision',()=>{
 const proposed=envelope({organization_id:org,aggregate_id:id,event_type:'relationship.proposed',aggregate_version:1});
 proposed.after.payload.change={id,subject:principal,manager:tenant,graph_version:2};
 const installed=envelope({id:tenant,organization_id:org,aggregate_id:id,event_type:'relationship.installed',aggregate_version:2});
 installed.after.payload.change={id,membership_id:tenant,version:1,graph_version:2};
 const a=parse(proposed),b=parse(installed);assert.equal(a.configuration_version,1);assert.equal(b.configuration_version,1);assert.equal(a.aggregate_version,1);assert.equal(b.aggregate_version,2);
});
test('new explicit administration and payroll job cancellation contracts remain closed',()=>{
 for(const operation of ['employee_detail','employee_create','employee_update','employee_employment','account_list','account_create','account_update','account_access','account_roles','workflow_list','workflow_detail','workflow_save','workflow_activation','separation_list','separation_detail','separation_create','separation_reject','separation_cancel_approval','clearance_update','payroll_job_list','payroll_job_detail','payroll_job_cancel_before_execution']){const e=envelope({organization_id:org,aggregate_id:id,event_type:'command.cancelled'});e.after.payload.change={command_id:id,resource_id:principal,membership_id:tenant,operation,version:1};assert.equal(parse(e).event_type,'command.cancelled');}
});
test('control sink acknowledgement precedes checkpoint and quarantine contains no input',async()=>{
 const calls=[];const history={write:async()=>calls.push('history'),checkpoint:async()=>calls.push('checkpoint')};
 const args={raw:raw(envelope()),topic:binding.topic,partition:0,offset:'0',binding,history,quarantine:async m=>calls.push(m)};
 await processControlRecord(args);assert.deepEqual(calls,['history','checkpoint']);
 calls.length=0;history.write=async()=>{throw Error('offline');};await assert.rejects(processControlRecord(args));assert.deepEqual(calls,[]);
 await processControlRecord({...args,raw:raw({private_secret:'never-log'})});assert.equal(calls.length,2);assert.equal(calls[0].code,'CONTROL_APPEND_ONLY');assert(!JSON.stringify(calls).includes('never-log'));
 calls.length=0;await assert.rejects(processControlRecord({...args,raw:raw({secret:'x'}),quarantine:async()=>{throw Error('DLQ offline');}}));assert.deepEqual(calls,[]);
});
test('eventID/version collision cannot overwrite history; quarantined checkpoint remains partial',async()=>{
 const event=parse(envelope()),calls=[];
 let prior='';const client={query:async(sql,params,body)=>{calls.push({sql,params,body});if(sql.startsWith('SELECT event_id'))return prior;return '';}};
 const history=new ControlHistory(client);await history.write(event);assert.match(calls.at(-1).sql,/async_insert=0/);
 prior=JSON.stringify({event_id:event.event_id,content_hash:'conflict'});await assert.rejects(history.write(event),e=>e.code==='CONTROL_HISTORY_CONFLICT');
 prior=JSON.stringify({event_id:tenant,content_hash:event.content_hash});await assert.rejects(history.write(event),e=>e.code==='CONTROL_HISTORY_CONFLICT');
 client.query=async(sql,params,body)=>sql.startsWith('SELECT state')?JSON.stringify({state:'QUARANTINED',first_quarantine_offset:'4'}):(calls.push({sql,params,body}), '');
 await history.checkpoint({topic:binding.topic,partition:0,offset:'8',state:'READY',code:''});const row=JSON.parse(calls.at(-1).body);assert.equal(row.state,'QUARANTINED');assert.equal(row.first_quarantine_offset,'4');
});
