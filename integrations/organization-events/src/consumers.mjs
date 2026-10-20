import {parseEvent,InvalidEvent,digest} from './events.mjs';
export const CONSUMER_ROLES=Object.freeze(['inbox','history','reporting']);
export function consumerGroup(base,role) {
 if(!CONSUMER_ROLES.includes(role)||!base||!/^[a-zA-Z0-9._-]{1,180}$/.test(base))throw Error('INVALID_CONSUMER_CONFIGURATION');
 return `${base}.${role}.v2`;
}
// Each invocation acknowledges exactly one failure domain. Only inbox owns the
// current projection; history/reporting own independent immutable CH stores.
export async function processConsumerRecord({raw,topic,partition,offset,bindings,store,history,quarantine,role}) {
 if(!CONSUMER_ROLES.includes(role))throw Error('CONSUMER_ROLE_REQUIRED');
 const position={topic,partition,offset,role};
 let e;
 try {e=parseEvent(raw,topic,bindings);}catch(error){
  if(!(error instanceof InvalidEvent))throw error;
  await quarantine({...position,code:error.code,sha256:digest(raw)});
  await store.markConsumerPartial({...position,binding:bindings[topic],code:error.code});
  return {quarantined:true};
 }
 return store.transaction(async tx=>{
  const membership=await tx.membershipClassification(e);
  if(membership==='NONE'){
   await quarantine({...position,code:'MEMBERSHIP_UNRECOGNIZED',sha256:digest(raw),event_id:e.event_id});
   return {quarantined:true,discarded:true};
  }
  const mark=(state,code)=>tx.checkpoint(e,{...position,state,code});
  if(!await tx.identity(e,role)) {
   await quarantine({...position,code:'VERSION_CONFLICT',sha256:digest(raw),event_id:e.event_id});
   await mark('QUARANTINED','VERSION_CONFLICT');return {quarantined:true};
  }
  if(role==='inbox') {
   // Handling retained history successfully is an acknowledgement, not loss.
   // Current coverage is separately fenced by live epoch, bootstrap and barriers.
   if(membership==='HISTORICAL'){await mark('READY',null);return {historical:true};}
   const current=await tx.lock(e);
   if(current&&Number(current.source_version)===e.aggregate_version&&current.payload_hash!==e.payload_hash){
    await quarantine({...position,code:'VERSION_CONFLICT',sha256:digest(raw),event_id:e.event_id});await mark('QUARANTINED','VERSION_CONFLICT');return {quarantined:true};
   }
   if(!current||Number(current.source_version)<e.aggregate_version)await tx.apply(e);
   await tx.watermark(e,{...position,state:'READY'});
  } else {
   try{await history.write(e);}catch(error){
    if(error.code!=='HISTORY_VERSION_CONFLICT')throw error;
    await quarantine({...position,code:error.code,sha256:digest(raw),event_id:e.event_id});await mark('QUARANTINED',error.code);return {quarantined:true};
   }
  }
  await mark('READY',null);
  return {quarantined:false,historical:membership==='HISTORICAL'};
 });
}
