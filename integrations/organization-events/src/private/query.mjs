import {createHmac,timingSafeEqual,createHash} from 'node:crypto';
import {uuid} from './contract.mjs';
const requestFields=['version','tenant_id','source_generation','request_id','issued_at','filter','coverage_id','coverage_sha256'];
const filterFields=['tenant_id','entity_type','entity_id','actions','since','until','offset','limit'];
const denied=()=>{throw Error('PRIVATE_HISTORY_PROOF_DENIED');};
function exact(value,fields){if(!value||typeof value!=='object'||Array.isArray(value)||Object.keys(value).length!==fields.length||!fields.every(k=>Object.hasOwn(value,k)))denied();}
function from64(value,max){if(typeof value!=='string'||!value||value.length>Math.ceil(max/3)*4||!/^[A-Za-z0-9_-]+$/.test(value))denied();const bytes=Buffer.from(value,'base64url');if(bytes.length>max||bytes.toString('base64url')!==value)denied();return bytes;}
function mac(key,direction,bytes,purpose){if(!['request','response'].includes(direction)||!['history','delivery-history'].includes(purpose))denied();return createHmac('sha256',key).update(`kabipay-private-${purpose}-${direction}-v1\0`).update(bytes).digest();}
export function encodeProof(value,key,direction,purpose='history'){const bytes=Buffer.from(JSON.stringify(value));return {payload:bytes.toString('base64url'),signature:mac(key,direction,bytes,purpose).toString('base64url')};}
export function decodeProof(proof,key,direction,purpose='history'){exact(proof,['payload','signature']);const bytes=from64(proof.payload,direction==='request'?16384:64*1024*1024),signature=from64(proof.signature,32);if(signature.length!==32||!timingSafeEqual(signature,mac(key,direction,bytes,purpose)))denied();try{return JSON.parse(bytes);}catch{denied();}}
export class PrivateHistoryQuery {
 constructor({bindings,store,now=()=>Math.floor(Date.now()/1000)}){
  if(!Array.isArray(bindings)||bindings.length<1||bindings.length>256)denied();this.bindings=new Map();this.replay=new Map();this.now=now;this.store=store;const used=new Set();
  for(const b of bindings){const key=Buffer.from(b.key_b64??'','base64');if(!uuid(b.tenant_id)||!Number.isSafeInteger(b.source_generation)||b.source_generation<1||key.length!==32||key.toString('base64')!==b.key_b64||this.bindings.has(b.tenant_id)||used.has(b.key_b64))denied();used.add(b.key_b64);this.bindings.set(b.tenant_id,{...b,key});}
 }
 async execute(proof){
  // Unverified decoding selects only a preconfigured key. No database, URL,
  // permission or tenant claim is used until the signature has passed.
  exact(proof,['payload','signature']);const bytes=from64(proof.payload,16384);let candidate;try{candidate=JSON.parse(bytes);}catch{denied();}
  const binding=this.bindings.get(candidate?.tenant_id);if(!binding)denied();const r=decodeProof(proof,binding.key,'request'),now=this.now();exact(r,requestFields);
  if(r.version!==1||r.tenant_id!==binding.tenant_id||r.source_generation!==binding.source_generation||!uuid(r.request_id)||!uuid(r.coverage_id)||!/^[a-f0-9]{64}$/.test(r.coverage_sha256)||!Number.isSafeInteger(r.issued_at)||r.issued_at<now-60||r.issued_at>now+5)denied();
  exact(r.filter,filterFields);const f=r.filter;
  if(f.tenant_id!==r.tenant_id||!Number.isSafeInteger(f.limit)||f.limit<1||f.limit>500||!Number.isSafeInteger(f.offset)||f.offset<0||f.offset>100000||!Array.isArray(f.actions)||f.actions.length>64||!f.actions.every(a=>typeof a==='string'&&a.length<=128)||!(f.entity_type===null||typeof f.entity_type==='string'&&f.entity_type.length<=128)||!(f.entity_id===null||uuid(f.entity_id)))denied();
  for(const k of ['since','until'])if(f[k]!==null&&(typeof f[k]!=='string'||!/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,9})?Z$/.test(f[k])||!Number.isFinite(Date.parse(f[k]))))denied();
  for(const [id,expires] of this.replay)if(expires<now)this.replay.delete(id);
  const replayId=`${r.tenant_id}/${r.request_id}`;if(this.replay.has(replayId)||this.replay.size>=10000)denied();this.replay.set(replayId,now+66);
  await this.store.requireCoverage({...r,source_database:binding.database,source_schema:binding.schema});
  const page=await this.store.readAudit({...f,source_database:binding.database,source_schema:binding.schema});
  if(!page||!Array.isArray(page.records)||page.records.length>f.limit||!Number.isSafeInteger(page.total)||page.total<0||page.records.some(raw=>typeof raw!=='string'||JSON.parse(raw).tenant_id!==r.tenant_id))throw Error('PRIVATE_HISTORY_STORAGE_SCOPE');
  const response={version:1,tenant_id:r.tenant_id,source_generation:r.source_generation,request_id:r.request_id,issued_at:this.now(),coverage_id:r.coverage_id,request_hash:createHash('sha256').update(bytes).digest('hex'),page};
  if(Buffer.byteLength(JSON.stringify(response))>8*1024*1024)throw Error('PRIVATE_HISTORY_RESPONSE_LIMIT');
  return encodeProof(response,binding.key,'response');
 }
}
export function privateHistoryHandler(service){
 return async(req,res)=>{
  if(req.method!=='POST'||req.url!=='/internal/private-events/history'){res.writeHead(404);res.end();return;}
  let size=0;const chunks=[];req.on('data',chunk=>{size+=chunk.length;if(size>24000){res.writeHead(413);res.end();req.destroy();}else chunks.push(chunk);});
  req.on('end',async()=>{if(size>24000)return;try{const result=await service.execute(JSON.parse(Buffer.concat(chunks)));res.writeHead(200,{'content-type':'application/json','cache-control':'no-store'});res.end(JSON.stringify(result));}catch(error){const denied=error?.message==='PRIVATE_HISTORY_PROOF_DENIED';res.writeHead(denied?403:503,{'content-type':'application/json','cache-control':'no-store'});res.end(JSON.stringify({error:denied?'ACCESS_DENIED':'HISTORY_UNAVAILABLE'}));}});
  req.on('error',()=>{if(!res.writableEnded)res.destroy();});
 };
}
