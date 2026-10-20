import {digest,stable,InvalidEvent} from './events.mjs';
import {COMMAND_OPERATIONS} from './operations.mjs';
const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const fail=code=>{throw new InvalidEvent(code);};
const check=(value,code)=>{if(!value)fail(code);};
const object=v=>v!==null&&typeof v==='object'&&!Array.isArray(v);
function exact(v,keys){check(object(v)&&Object.keys(v).length===keys.length&&keys.every(k=>Object.hasOwn(v,k)),'CONTROL_FIELD_SET');}
function id(v){check(typeof v==='string'&&uuid.test(v),'CONTROL_UUID');}
function version(v){check(Number.isSafeInteger(v)&&v>0,'CONTROL_VERSION');}
function text(v,max=255){check(typeof v==='string'&&[...v].length<=max&&!/[\u0000-\u001f]/.test(v),'CONTROL_TEXT');}
function status(v){check(typeof v==='string'&&/^[A-Z][A-Z_]{0,39}$/.test(v),'CONTROL_STATUS');}
function fields(v,types){exact(v,Object.keys(types));for(const [key,validate]of Object.entries(types))validate(v[key]);}
function ids(v){check(Array.isArray(v)&&v.length<=100,'CONTROL_ARRAY');v.forEach(id);}
function capabilities(v){check(Array.isArray(v)&&v.length<=100,'CONTROL_ARRAY');for(const c of v){fields(c,{code:v=>{check(typeof v==='string'&&/^[a-z][a-z0-9_-]*:[a-z][a-z0-9_-]*$/.test(v)&&v.length<=100,'CONTROL_CAPABILITY');},employee_scope:v=>check(['SELF_ONLY','TEAM','DEPARTMENT','ALL'].includes(v),'CONTROL_SCOPE')});}}
function request(v){
 check(object(v),'CONTROL_REQUEST');
 const schemas={invite_company:{tenant:id},accept_company:{membership:id,version},suspend_company:{membership:id,version},resume_company:{membership:id,version},begin_detach_company:{membership:id,version},create_role:{name:text,capabilities},replace_role:{role:id,version,capabilities},grant_role:{principal:id,role:id,companies:ids},revoke_binding:{binding:id,version}};
 const schema=schemas[v.operation];check(schema,'CONTROL_OPERATION');fields(v,{operation:()=>{},...schema});
}
function change(kind,v){
 const schemas={
  'identity.registered':{principal_id:id,link_id:id,display_name:text,status},
  'identity.revoked':{link_id:id,status},
  'organization.bootstrapped':{organization_id:id,name:text,membership_id:id,role_id:id,binding_id:id,capabilities:v=>{check(Array.isArray(v)&&v.length<=100,'CONTROL_ARRAY');v.forEach(x=>check(typeof x==='string'&&/^[a-z][a-z0-9_-]*:[a-z][a-z0-9_-]*$/.test(x),'CONTROL_CAPABILITY'));},status},
  'organization.configuration_changed':{request,id,status,version,organization_version:version},
  'relationship.proposed':{id,subject:id,manager:id,graph_version:version},
  'relationship.manager_changed':{id,manager:id,version,graph_version:version},
  'relationship.ended':{id,status,graph_version:version},
  'relationship.installed':{id,membership_id:id,version,graph_version:version},
  'grant.installed':{membership_id:id,binding_id:id,version},
  'membership.installed':{membership_id:id,status,version},
  'command.cancelled':{command_id:id,resource_id:id,membership_id:id,operation:v=>check(COMMAND_OPERATIONS.includes(v),'CONTROL_COMMAND_OPERATION'),version},
  'directory.synchronized':{membership_id:id,records:v=>check(Number.isSafeInteger(v)&&v>=0&&v<=1000,'CONTROL_COUNT'),source_after:v=>{if(v!==null)id(v);},has_more:v=>check(typeof v==='boolean','CONTROL_BOOLEAN'),source_bootstrap:v=>{if(v!==null)fields(v,{status:v=>check(['PENDING','COMPLETE','PAUSED','STOPPED'].includes(v),'CONTROL_BOOTSTRAP'),phase:v=>check(Number.isInteger(v)&&v>=0&&v<=5,'CONTROL_PHASE')});}},
 };
 check(schemas[kind],'CONTROL_EVENT_TYPE');fields(v,schemas[kind]);
}

// Separate trust domain: only the fixed metadata connector may produce this
// topic. Native envelopes remain valid during rolling stream-router deployment.
export function parseControlEvent(raw,topic,binding){
 check(Buffer.isBuffer(raw)&&raw.length<=262144,'CONTROL_EVENT_SIZE');
 check(binding&&topic===binding.topic,'CONTROL_TOPIC_PROVENANCE');
 let envelope,wrapped=false;try{envelope=JSON.parse(raw.toString('utf8'));if(typeof envelope==='string'){wrapped=true;check(Buffer.byteLength(envelope)<=262144,'CONTROL_EVENT_SIZE');envelope=JSON.parse(envelope);}}catch{fail('CONTROL_JSON');}
 if(wrapped)check(object(envelope)&&Object.hasOwn(envelope,'route_schema_version'),'CONTROL_ROUTE_SCHEMA');
 let e;
 if(object(envelope)&&Object.hasOwn(envelope,'route_schema_version')){
  exact(envelope,['route_schema_version','source_database','source_schema','event']);
  check(envelope.route_schema_version===1,'CONTROL_ROUTE_SCHEMA');
  check(envelope.source_database===binding.database&&envelope.source_schema===binding.schema,'CONTROL_SOURCE_PROVENANCE');
  e=envelope.event;
 }else{
  check(object(envelope)&&['c','r'].includes(envelope.op),'CONTROL_APPEND_ONLY');
  const source=envelope.source;
  check(object(source)&&source.db===binding.database&&source.schema===binding.schema&&source.table==='organization_outbox'&&source.name===binding.connector_name,'CONTROL_SOURCE_PROVENANCE');
  e=envelope.after;
 }
 exact(e,['id','organization_id','source_tenant_id','actor_principal_id','aggregate_id','aggregate_version','event_type','schema_version','payload','created_at']);
 for(const key of ['id','source_tenant_id','actor_principal_id','aggregate_id'])id(e[key]);
 if(e.organization_id!==null)id(e.organization_id);
 check(e.schema_version===1,'CONTROL_SCHEMA');version(e.aggregate_version);
 check(typeof e.created_at==='string'&&/^\d{4}-\d\d-\d\dT/.test(e.created_at)&&Number.isFinite(Date.parse(e.created_at)),'CONTROL_TIMESTAMP');
 let p=e.payload;if(typeof p==='string'){try{p=JSON.parse(p);}catch{fail('CONTROL_PAYLOAD_JSON');}}
 fields(p,{actor:v=>fields(v,{principal_id:id,source_tenant_id:id,source_user_id:id,issuer:v=>check(v==='kabipay-client','CONTROL_ISSUER')}),configuration_version:version,change:v=>change(e.event_type,v)});
 check(p.actor.principal_id===e.actor_principal_id&&p.actor.source_tenant_id===e.source_tenant_id,'CONTROL_ACTOR_PROVENANCE');
 check(e.event_type.startsWith('identity.')?e.organization_id===null:e.organization_id!==null,'CONTROL_ORGANIZATION_SCOPE');
 if(e.event_type==='identity.registered')check(p.change.principal_id===e.aggregate_id&&p.change.principal_id===e.actor_principal_id,'CONTROL_AGGREGATE');
 if(e.event_type==='identity.revoked')check(p.change.link_id===e.aggregate_id,'CONTROL_AGGREGATE');
 if(e.event_type==='organization.bootstrapped')check(p.change.organization_id===e.organization_id&&e.aggregate_id===e.organization_id,'CONTROL_AGGREGATE');
 if(Object.hasOwn(p.change,'id'))check(p.change.id===e.aggregate_id,'CONTROL_AGGREGATE');
 if(e.event_type==='grant.installed')check(p.change.binding_id===e.aggregate_id,'CONTROL_AGGREGATE');
 if(['membership.installed','directory.synchronized'].includes(e.event_type))check(p.change.membership_id===e.aggregate_id,'CONTROL_AGGREGATE');
 if(e.event_type==='command.cancelled')check(p.change.command_id===e.aggregate_id&&p.change.version===p.configuration_version,'CONTROL_AGGREGATE');
 const result={event_id:e.id,organization_id:e.organization_id,source_tenant_id:e.source_tenant_id,actor_principal_id:e.actor_principal_id,aggregate_id:e.aggregate_id,aggregate_version:e.aggregate_version,schema_version:1,event_type:e.event_type,occurred_at:e.created_at,configuration_version:p.configuration_version,actor:stable(p.actor),change:stable(p.change),metadata_source:binding.connector_name};
 return {...result,content_hash:digest(result)};
}

export async function processControlRecord({raw,topic,partition,offset,binding,history,quarantine}){
 let event;
 const position={topic,partition,offset};
 async function reject(code){await quarantine({...position,code,sha256:digest(raw)});await history.checkpoint({...position,state:'QUARANTINED',code});return {quarantined:true};}
 try{event=parseControlEvent(raw,topic,binding);}catch(error){if(!(error instanceof InvalidEvent))throw error;return reject(error.code);}
 try{await history.write(event);}catch(error){if(error.code!=='CONTROL_HISTORY_CONFLICT')throw error;return reject(error.code);}
 await history.checkpoint({...position,state:'READY',code:''});
 return {quarantined:false};
}
