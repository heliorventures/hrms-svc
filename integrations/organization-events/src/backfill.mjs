import {parseEvent,digest} from './events.mjs';
function requireValue(ok){if(!ok)throw Error('BACKFILL_RECONCILIATION_FAILED');}
export function verifyExportChunk({rawManifest,expectedSha256,scope,topic,binding,previous}){
 requireValue(typeof rawManifest==='string'&&Buffer.byteLength(rawManifest)<=16*1024*1024&&digest(rawManifest)===expectedSha256);const m=JSON.parse(rawManifest);
 requireValue(m.schema_version===1&&m.canonicalization==='POSTGRES_JSONB_TEXT_UTF8_V1'&&['OUTBOX','LEGACY_AUDIT'].includes(m.source_kind)&&m.tenant_id===scope.source_tenant_id&&m.organization_id===scope.organization_id&&m.membership_id===scope.membership_id&&m.membership_version===scope.membership_version);
 for(const n of [m.chunk_index,m.after_position,m.through_position,m.cutoff_position,m.row_count])requireValue(Number.isSafeInteger(n)&&n>=0);
 requireValue(m.chunk_index===previous.chunk_index+1&&m.after_position===previous.through_position&&m.through_position>=m.after_position&&m.through_position<=m.cutoff_position&&Array.isArray(m.rows)&&m.rows.length===m.row_count&&m.rows.length<=200);
 let position=m.after_position;const ids=new Set(),events=[];
 for(const row of m.rows){requireValue(Number.isSafeInteger(row.source_position)&&row.source_position>position&&row.source_position<=m.through_position&&typeof row.raw==='string'&&digest(row.raw)===row.sha256&&!ids.has(row.id));position=row.source_position;ids.add(row.id);const e=parseEvent(Buffer.from(row.raw),topic,{[topic]:binding});requireValue(m.source_kind==='LEGACY_AUDIT'?e.event_type==='legacy_audit.recorded'&&e.payload.source_audit_id===row.id:e.event_id===row.id);const {payload_hash,...event}=e;events.push(event);}
 requireValue(m.rows.length===0?m.through_position===m.cutoff_position:position===m.through_position);
 return {manifest:m,events};
}
