import { createHash } from 'node:crypto';
export const MAX_SOURCE_BYTES=16*1024*1024, FRAME_BYTES=256*1024;
export const HEAD_FIELDS=['schema_version','event_id','record_id','tenant_id','purpose','source_generation','source_kind','resource_type','resource_id','occurred_at','key_id','source_sha256','source_size','frame_index','frame_count'];
export const fail=()=>{throw new Error('invalid private event');};
export const uuid=v=>typeof v==='string'&&/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(v)&&v!=='00000000-0000-0000-0000-000000000000';
export const identifier=v=>typeof v==='string'&&/^[A-Za-z0-9_.:-]{1,128}$/.test(v);
export const purpose=v=>['audit','delivery','operator_audit'].includes(v);
export const sourceHash=b=>createHash('sha256').update(b).digest('hex');
export function decode64(value,max){if(typeof value!=='string'||value.length>Math.ceil(max/3)*4)fail();const b=Buffer.from(value,'base64');if(b.length>max||b.toString('base64')!==value)fail();return b;}
export function validateHead(h){if(!h||h.schema_version!==1||!['event_id','record_id','tenant_id','resource_id'].every(k=>uuid(h[k]))||!purpose(h.purpose)||!Number.isSafeInteger(h.source_generation)||h.source_generation<1||!['source_kind','resource_type','key_id'].every(k=>identifier(h[k]))||!/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{6}Z$/.test(h.occurred_at)||!Number.isFinite(Date.parse(h.occurred_at))||new Date(h.occurred_at).toISOString()!==h.occurred_at.slice(0,23)+'Z'||!/^[0-9a-f]{64}$/.test(h.source_sha256)||!Number.isInteger(h.source_size)||h.source_size<1||h.source_size>MAX_SOURCE_BYTES||!Number.isInteger(h.frame_count)||h.frame_count!==Math.ceil(h.source_size/FRAME_BYTES)||!Number.isInteger(h.frame_index)||h.frame_index<0||h.frame_index>=h.frame_count)fail();return h;}
export function associatedData(h){validateHead(h);return Buffer.concat([Buffer.from('kabipay-private-event-v1\0'),...HEAD_FIELDS.map(k=>{const b=Buffer.from(String(h[k]));return Buffer.concat([Buffer.from(`${b.length}:`),b]);})]);}
