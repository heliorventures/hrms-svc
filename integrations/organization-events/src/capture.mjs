import {CONSUMER_ROLES} from './consumers.mjs';
import {parseCdcJson} from './cdc-json.mjs';
import {digest} from './events.mjs';
const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
function requireValue(ok){if(!ok)throw Error('INVALID_CAPTURE_PROOF');}
function exact(o,keys){requireValue(o&&typeof o==='object'&&!Array.isArray(o)&&keys.every(k=>Object.hasOwn(o,k))&&Object.keys(o).every(k=>keys.includes(k)));}
export function validatePartitions(partitions){requireValue(Array.isArray(partitions)&&partitions.length>0&&partitions.length<=128&&partitions.every(p=>Number.isSafeInteger(p)&&p>=0)&&new Set(partitions).size===partitions.length);return partitions;}
function lsnNumber(lsn){requireValue(typeof lsn==='string'&&/^[0-9a-f]{1,8}\/[0-9a-f]{1,8}$/i.test(lsn));const [high,low]=lsn.split('/');return (BigInt(`0x${high}`)<<32n)+BigInt(`0x${low}`);}
function cdcLsn(value){requireValue((Number.isSafeInteger(value)&&value>=0)||(typeof value==='string'&&/^\d+$/.test(value)));const n=BigInt(value);return `${(n>>32n).toString(16).toUpperCase()}/${(n&0xffffffffn).toString(16).toUpperCase()}`;}
export function parseCaptureMarker(raw,binding){
 requireValue(raw.length<=16384);const e=parseCdcJson(raw.toString());requireValue(e&&typeof e==='object'&&!Array.isArray(e));const r=e.after;
 requireValue(e.op==='c'&&e.source?.db===binding.source_database&&e.source.schema===binding.source_schema&&e.source.table==='organization_capture_marker'&&(!binding.topic_prefix||e.source.name===binding.topic_prefix));
 exact(r,['marker_id','organization_id','tenant_id','membership_id','membership_version','source_marker_at','source_lsn','bootstrap_complete']);requireValue(typeof r.bootstrap_complete==='boolean');
 for(const k of ['marker_id','organization_id','tenant_id','membership_id'])requireValue(typeof r[k]==='string'&&uuid.test(r[k]));
 requireValue(r.tenant_id===binding.tenant_id&&binding.organization_ids.includes(r.organization_id)&&Number.isSafeInteger(r.membership_version)&&r.membership_version>0);
 requireValue(typeof r.source_marker_at==='string'&&Number.isFinite(Date.parse(r.source_marker_at)));
 const cdc_lsn=cdcLsn(e.source.lsn);requireValue(lsnNumber(cdc_lsn)>=lsnNumber(r.source_lsn));
 return {...r,source_tenant_id:r.tenant_id,cdc_lsn};
}
export function barriersForMarker(marker,binding){return validatePartitions(binding.expected_partitions).map(partition=>({topic:binding.company_topic,partition,key:marker.marker_id,value:JSON.stringify({schema_version:1,event_type:'capture.barrier',marker_id:marker.marker_id,organization_id:marker.organization_id,source_tenant_id:marker.tenant_id,membership_id:marker.membership_id,membership_version:marker.membership_version})}));}
export function parseBarrier(raw){const e=JSON.parse(raw.toString());exact(e,['schema_version','event_type','marker_id','organization_id','source_tenant_id','membership_id','membership_version']);requireValue(e.schema_version===1&&e.event_type==='capture.barrier'&&Number.isSafeInteger(e.membership_version)&&e.membership_version>0);for(const k of ['marker_id','organization_id','source_tenant_id','membership_id'])requireValue(typeof e[k]==='string'&&uuid.test(e[k]));return e;}
export function coverage({marker,binding,membershipVersion,observations,now=Date.now(),maxAgeMs=120000,roles=CONSUMER_ROLES}){
 const missing=[];if(marker.membership_version!==membershipVersion)missing.push('MEMBERSHIP_EPOCH');if(!marker.bootstrap_complete)missing.push('SOURCE_BOOTSTRAP_INCOMPLETE');
 const age=now-Date.parse(marker.source_marker_at);if(!Number.isFinite(age)||age>maxAgeMs||age< -5000)missing.push('STALE_SOURCE_MARKER');
 if(!marker.cdc_lsn||lsnNumber(marker.cdc_lsn)<lsnNumber(marker.source_lsn))missing.push('CDC_NOT_CONFIRMED');
 requireValue(roles.length>0&&roles.every(r=>CONSUMER_ROLES.includes(r)));
 for(const role of roles)for(const partition of validatePartitions(binding.expected_partitions))if(!observations.some(o=>o.role===role&&o.partition===partition&&o.marker_id===marker.marker_id&&o.state==='READY'))missing.push(`${role}:${partition}`);
 return {complete:missing.length===0,missing};
}
// Called ONLY for the fixed raw CDC marker topic. Register before fanout; a
// crash can leave missing barriers, which cannot make coverage complete.
export async function coordinateMarker({raw,binding,pool,producer,confirmCapture,quarantine}){
 const reject=async code=>{
  if(typeof quarantine!=='function')throw Error(code);
  await quarantine({code,sha256:digest(raw)});return {quarantined:true};
 };
 let m;try{m=parseCaptureMarker(raw,binding);}catch(error){
  if(!['INVALID_CAPTURE_PROOF','CDC_OFFSET_INTEGER_REQUIRED'].includes(error.message)&&!(error instanceof SyntaxError))throw error;
  return reject('INVALID_CAPTURE_PROOF');
 }
 const c=await pool.connect();
 let inTransaction=false;
 const begin=async()=>{await c.query('BEGIN');inTransaction=true;};
 const commit=async()=>{await c.query('COMMIT');inTransaction=false;};
 const terminal=async()=>{
  const known=(await c.query('SELECT kabipay_ops.organization_capture_marker_classification($1,$2,$3,$4) AS state',[m.organization_id,m.tenant_id,m.membership_id,m.membership_version])).rows[0]?.state;
  if(!['ACTIVE','HISTORICAL','NONE'].includes(known))throw Error('MEMBERSHIP_CLASSIFICATION_UNAVAILABLE');
  if(known==='HISTORICAL'){await commit();return {...m,historical:true};}
  if(known==='NONE'){const result=await reject('CAPTURE_MEMBERSHIP_UNRECOGNIZED');await commit();return result;}
  return null;
 };
 try{
  // Retired historical connectors must not gate historical ACK/quarantine.
  // No live-policy lock is held over Connect I/O; recheck the exact epoch under
  // fresh locks before any current registration or barrier side effect.
  await begin();let result=await terminal();if(result)return result;await commit();
  // A persisted source checkpoint proves earlier cross-topic sends were ACKed.
  if(typeof confirmCapture!=='function'||await confirmCapture(m,binding)!==true)throw Error('CONNECT_CAPTURE_NOT_CONFIRMED');
  await begin();result=await terminal();if(result)return result;
  await c.query(`INSERT INTO kabipay_ops.organization_capture_marker(marker_id,organization_id,source_tenant_id,membership_id,membership_version,topic,source_marker_at,source_lsn,cdc_lsn,expected_partitions,bootstrap_complete) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT(marker_id) DO NOTHING`,[m.marker_id,m.organization_id,m.tenant_id,m.membership_id,m.membership_version,binding.company_topic,m.source_marker_at,m.source_lsn,m.cdc_lsn,validatePartitions(binding.expected_partitions),m.bootstrap_complete]);
  const existing=(await c.query('SELECT * FROM kabipay_ops.organization_capture_marker WHERE marker_id=$1',[m.marker_id])).rows[0];
  if(existing.organization_id!==m.organization_id||existing.source_tenant_id!==m.tenant_id||existing.membership_id!==m.membership_id||Number(existing.membership_version)!==m.membership_version||existing.topic!==binding.company_topic||existing.bootstrap_complete!==m.bootstrap_complete||new Date(existing.source_marker_at).getTime()!==Date.parse(m.source_marker_at)||lsnNumber(existing.source_lsn)!==lsnNumber(m.source_lsn))throw Error('CAPTURE_MARKER_CONFLICT');
  await commit();
 }catch(error){if(inTransaction)await c.query('ROLLBACK');throw error;}finally{c.release();}
 const messages=barriersForMarker(m,binding);await producer.send({topic:binding.company_topic,messages:messages.map(({topic,...message})=>message),acks:-1});return m;
}
