import {TextDecoder} from 'node:util';
import {decryptFrame,decryptFrames} from './crypto.mjs';
import {sourceHash,uuid,purpose as validPurpose} from './contract.mjs';
const decoder=new TextDecoder('utf-8',{fatal:true});
const tables=Object.freeze({audit_log:'audit_log',legacy_audit_log:'audit_log',legacy_outbox_event:'outbox_event',legacy_webhook_delivery_log:'webhook_delivery_log',legacy_employee_finance_outbox:'employee_finance_outbox',operator_audit_log:'operator_audit_log',legacy_operator_audit_log:'operator_audit_log',webhook:'private_delivery_receipt',finance_confirmation:'private_delivery_receipt',finance_cancellation:'private_delivery_receipt',delivery_outcome:'private_delivery_receipt',capture_marker:'private_capture_marker',delivery_replay:'delivery_replay_command'});
const executable=new Set(['webhook','finance_confirmation','finance_cancellation']);
const datasets=Object.freeze({audit:new Set(['audit_log','legacy_audit_log','capture_marker']),delivery:new Set([...executable,'legacy_outbox_event','legacy_webhook_delivery_log','legacy_employee_finance_outbox','delivery_replay','capture_marker']),operator_audit:new Set(['operator_audit_log','legacy_operator_audit_log','capture_marker'])});
function denied(code='PRIVATE_HISTORY_INVALID'){throw Error(code);}
export function privateBinding(input){
 if(!input||!uuid(input.tenant_id)||!validPurpose(input.purpose)||!Number.isSafeInteger(input.source_generation)||input.source_generation<1||!['database','schema'].every(k=>typeof input[k]==='string'&&/^[A-Za-z0-9_][A-Za-z0-9_-]{0,127}$/.test(input[k])))denied();
 if(input.mode!==undefined&&input.mode!=='outcome')denied();
 if(input.mode==='outcome'&&input.purpose!=='delivery')denied();
 const topic=input.mode==='outcome'?`organization.delivery.history.${input.tenant_id}`:input.purpose==='operator_audit'?'organization.operator_audit.metadata':`organization.${input.purpose}.${input.tenant_id}`;
 if(input.topic!==topic)denied();
 return Object.freeze({...input});
}
function assertSameRecord(a,b){
 for(const key of ['scope_id','source_database','source_schema','source_table','record_id','source_sha256','source_size','source_kind','resource_type','resource_id','occurred_at'])if(String(a[key])!==String(b[key]))denied('PRIVATE_HISTORY_CONFLICT');
}
export function privateRecord(head,bytes,binding){
 let raw_source,row;try{raw_source=decoder.decode(bytes);row=JSON.parse(raw_source);}catch{denied();}
 if(!row||typeof row!=='object'||Array.isArray(row)||row.id!==head.record_id)denied();
 const originalTenant=row.tenant_id??null;
 if(binding.purpose!=='operator_audit'&&originalTenant!==binding.tenant_id)denied();
 if(originalTenant!==null&&!uuid(originalTenant))denied();
 if(head.source_kind==='capture_marker'){
  const fields=['id','tenant_id','source_generation','purpose','through_position','source_lsn','created_at'];
  if(Object.keys(row).length!==fields.length||!fields.every(k=>Object.hasOwn(row,k))||head.resource_type!=='private_capture_marker'||head.resource_id!==head.record_id||row.source_generation!==head.source_generation||row.purpose!==binding.purpose||row.created_at!==head.occurred_at||typeof row.through_position!=='string'||!/^\d{1,19}$/.test(row.through_position)||BigInt(row.through_position)>9223372036854775807n||typeof row.source_lsn!=='string'||!/^[0-9A-F]+\/[0-9A-F]+$/.test(row.source_lsn))denied('PRIVATE_CAPTURE_MARKER_INVALID');
 }
 const record={scope_id:binding.tenant_id,tenant_id:originalTenant,record_id:head.record_id,source_database:binding.database,source_schema:binding.schema,source_table:tables[head.source_kind]??'unknown',source_generation:head.source_generation,source_kind:head.source_kind,resource_type:head.resource_type,resource_id:head.resource_id,occurred_at:head.occurred_at.replace('T',' ').replace(/Z$/,''),source_sha256:head.source_sha256,source_size:bytes.length,raw_source,actor_id:row.user_id??row.operator_user_id??null,action:row.action??'',entity_type:row.entity_type??''};
 if(record.actor_id!==null&&!uuid(record.actor_id))denied();
 if(typeof record.action!=='string'||typeof record.entity_type!=='string')denied();
 return {record,row};
}
function deliveryReference(row,head,binding){
 if(binding.purpose!=='delivery'||!executable.has(head.source_kind))return null;
 if(head.source_generation!==binding.source_generation)denied('PRIVATE_DELIVERY_STALE_GENERATION');
 if(!uuid(row.destination_id)||!uuid(row.resource_id)||row.resource_id!==head.resource_id||row.source_kind!==head.source_kind||!Number.isSafeInteger(row.destination_version)||row.destination_version<1||typeof row.event_type!=='string'||row.event_type.length>128)denied();
 return {tenant_id:binding.tenant_id,intent_id:head.record_id,destination_id:row.destination_id,source_generation:head.source_generation,destination_version:row.destination_version,intent_hash:head.source_sha256};
}
function replayReference(row,head,binding){
 const p=row.payload,fields=['intent_id','intent_hash','destination_id','destination_version','source_generation','base_lease','retry_budget'];
 if(binding.purpose!=='delivery'||head.source_generation!==binding.source_generation||head.resource_type!=='delivery_replay'||row.resource_id!==head.resource_id||row.source_kind!=='delivery_replay'||row.event_type!=='private.delivery.replay.v1'||!p||Object.keys(p).length!==fields.length||!fields.every(k=>Object.hasOwn(p,k))||p.intent_id!==head.resource_id||!uuid(p.intent_id)||!uuid(p.destination_id)||p.destination_id!==row.destination_id||!Number.isSafeInteger(p.destination_version)||p.destination_version<1||p.destination_version!==row.destination_version||p.source_generation!==head.source_generation||!/^[a-f0-9]{64}$/.test(p.intent_hash)||!Number.isSafeInteger(p.base_lease)||p.base_lease<0||p.base_lease>Number.MAX_SAFE_INTEGER-4||!Number.isInteger(p.retry_budget)||p.retry_budget<0||p.retry_budget>3)denied('PRIVATE_REPLAY_INVALID');
 return {reference:{tenant_id:binding.tenant_id,intent_id:p.intent_id,destination_id:p.destination_id,source_generation:p.source_generation,destination_version:p.destination_version,intent_hash:p.intent_hash},options:{baseLease:p.base_lease,retryBudget:p.retry_budget}};
}
/** A single configured consumer serializes all partitions for a scope/purpose.
 * Durable partial chunks may advance Kafka ownership; they never prove a complete
 * record or allow source cleanup. No database credentials are accepted here. */
export class PrivateHistoryProcessor {
 constructor({store,keys,binding}){this.store=store;this.keys=keys;this.binding=privateBinding(binding);}
 async accept({raw,topic,partition,offset,sourcePosition}){
  const b=this.binding;
  if(topic!==b.topic||partition!==0||typeof offset!=='string'||!/^\d{1,19}$/.test(offset)||BigInt(offset)>9223372036854775807n||typeof sourcePosition!=='string'||!/^\d{1,19}$/.test(sourcePosition)||BigInt(sourcePosition)<(b.mode==='outcome'?0n:1n)||BigInt(sourcePosition)>9223372036854775807n||!Buffer.isBuffer(raw)||raw.length>400000)denied();
  let h;try{h=JSON.parse(decoder.decode(raw));if(typeof h==='string')h=JSON.parse(h);}catch{denied();}
  const bytes=decryptFrame(h,this.keys,b.tenant_id,b.purpose);
  if(b.mode==='outcome'?h.source_kind!=='delivery_outcome':!datasets[b.purpose].has(h.source_kind))denied('PRIVATE_DATASET_SCOPE');
  if(!Number.isSafeInteger(h.source_generation)||h.source_generation<1||h.source_generation>b.source_generation)denied('PRIVATE_HISTORY_GENERATION');
  const chunk={scope_id:b.tenant_id,record_id:h.record_id,event_id:h.event_id,source_kind:h.source_kind,resource_type:h.resource_type,frame_index:h.frame_index,frame_count:h.frame_count,source_sha256:h.source_sha256,source_generation:h.source_generation,source_position:sourcePosition,key_id:h.key_id,envelope:JSON.stringify(h),frame_sha256:sourceHash(bytes),raw_frame:bytes.toString('base64')};
  const existing=await this.store.chunk(chunk);
  if(existing){
   for(const key of ['source_sha256','source_generation','frame_count','key_id','frame_sha256','raw_frame'])if(String(existing[key])!==String(chunk[key]))denied('PRIVATE_HISTORY_CONFLICT');
   const prior=JSON.parse(existing.envelope);
   // Event IDs/nonces can differ after a source exporter retries; authenticated
   // logical frame identity and content must still agree exactly.
   for(const key of ['record_id','tenant_id','purpose','source_kind','resource_type','resource_id','occurred_at','source_size'])if(prior[key]!==h[key])denied('PRIVATE_HISTORY_CONFLICT');
  }else await this.store.writeChunk(chunk);
  const chunks=await this.store.frames(chunk);
  let complete=false,delivery=null,workOptions=null;
  if(chunks.length===h.frame_count){
   const all=chunks.map(c=>JSON.parse(c.envelope)),source=decryptFrames(all,this.keys,b.tenant_id,b.purpose);
   const {record,row}=privateRecord(h,source,b);
   record.source_position=chunks.reduce((max,c)=>BigInt(c.source_position)>BigInt(max)?c.source_position:max,'0');
   if(h.source_kind==='delivery_replay'){
    if(b.mode==='outcome')denied();const replay=replayReference(row,h,b);delivery=replay.reference;workOptions=replay.options;
   }else delivery=b.mode==='outcome'?null:deliveryReference(row,h,b);
   const previous=await this.store.record(record);
   if(previous)assertSameRecord(previous,record);else await this.store.writeRecord(record);
   complete=true;
  }else if(chunks.length>h.frame_count)denied('PRIVATE_HISTORY_CONFLICT');
  await this.store.checkpoint({scope_id:b.tenant_id,source_generation:h.source_generation,topic,partition_id:partition,kafka_offset:offset,record_id:h.record_id,source_sha256:h.source_sha256});
  return {complete,record_id:h.record_id,delivery,workOptions};
 }
}
