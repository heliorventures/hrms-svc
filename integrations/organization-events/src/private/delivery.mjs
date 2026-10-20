import {createHash,createHmac,timingSafeEqual} from 'node:crypto';
import https from 'node:https';
import {uuid} from './contract.mjs';
import {signOwnership,verifyOwnershipResponse} from './ownership.mjs';
const refs=['tenant_id','intent_id','destination_id','source_generation','destination_version','intent_hash'];
const requestFields=['version',...refs,'lease_generation','request_id','issued_at'];
const responseFields=[...requestFields,'status','outcome_code','acknowledgement_id'];
const states=['PENDING','PROCESSING','DELIVERED','DISABLED','RETRYABLE','PERMANENT','UNCERTAIN','NO_DESTINATION'];
const fail=()=>{throw Error('PRIVATE_DELIVERY_INVALID');};
function exact(o,fields){if(!o||typeof o!=='object'||Array.isArray(o)||Object.keys(o).length!==fields.length||!fields.every(k=>Object.hasOwn(o,k)))fail();}
function integer(v,min=0,max=Number.MAX_SAFE_INTEGER){return Number.isSafeInteger(v)&&v>=min&&v<=max;}
function validReference(r){exact(r,refs);if(!['tenant_id','intent_id','destination_id'].every(k=>uuid(r[k]))||!integer(r.source_generation,1)||!integer(r.destination_version,1)||!/^[a-f0-9]{64}$/.test(r.intent_hash))fail();}
const ordered=(v,fields)=>Object.fromEntries(fields.map(k=>[k,v[k]]));
function signature(value,key,direction,fields){return createHmac('sha256',key).update(`kabipay-private-delivery-${direction}-v1\0`).update(JSON.stringify(ordered(value,fields))).digest();}
function validRequest(r){exact(r,requestFields);validReference(Object.fromEntries(refs.map(k=>[k,r[k]])));if(r.version!==1||!integer(r.lease_generation)||!uuid(r.request_id)||!integer(r.issued_at))fail();}
export function signRequest(request,key){validRequest(request);return {request:ordered(request,requestFields),signature:signature(request,key,'request',requestFields).toString('base64url')};}
export function signResponse(response,key){return {response:ordered(response,responseFields),signature:signature(response,key,'response',responseFields).toString('base64url')};}
export function verifyResponse(envelope,request,key,now=Math.floor(Date.now()/1000)){
 exact(envelope,['response','signature']);const r=envelope.response;exact(r,responseFields);
 if(typeof envelope.signature!=='string'||!/^[A-Za-z0-9_-]{43}$/.test(envelope.signature))fail();const sig=Buffer.from(envelope.signature,'base64url');
 if(sig.toString('base64url')!==envelope.signature||!timingSafeEqual(sig,signature(r,key,'response',responseFields)))fail();
 for(const field of ['version',...refs,'request_id'])if(r[field]!==request[field])fail();
 if(!integer(r.lease_generation)||!integer(r.issued_at)||r.issued_at<now-60||r.issued_at>now+5||!states.includes(r.status)||typeof r.outcome_code!=='string'||!/^[A-Za-z0-9_.:-]{1,80}$/.test(r.outcome_code)||!(r.acknowledgement_id===null||uuid(r.acknowledgement_id))||r.status==='DELIVERED'&&!uuid(r.acknowledgement_id))fail();
 return r;
}
function requestId(ref,lease){const digest=createHash('sha256').update('kabipay-broker-delivery-request-v1\0').update(JSON.stringify(ordered(ref,refs))).update(`\0${lease}`).digest();digest[6]=(digest[6]&15)|0x80;digest[8]=(digest[8]&63)|0x80;const s=digest.subarray(0,16).toString('hex');return `${s.slice(0,8)}-${s.slice(8,12)}-${s.slice(12,16)}-${s.slice(16,20)}-${s.slice(20)}`;}
export function newWork(ref,now=Math.floor(Date.now()/1000),{baseLease=0,retryBudget=3}={}){validReference(ref);if(!integer(baseLease,0,Number.MAX_SAFE_INTEGER-4)||!integer(retryBudget,0,3))fail();return {version:1,ref:ordered(ref,refs),base_lease:baseLease,retry_budget:retryBudget,lease_generation:baseLease+1,attempt:0,probes:0,available_at:now};}
export class SourceDeliveryClient {
 constructor({url,key,ca,cert,privateKey,timeout=30000}){
  this.url=new URL(url);if(this.url.protocol!=='https:'||this.url.username||this.url.password||this.url.hash||this.url.search||this.url.pathname!=='/internal/private-events/delivery'||!Buffer.isBuffer(key)||key.length<32||key.length>128||Boolean(cert)!==Boolean(privateKey))fail();
  this.key=key;this.timeout=timeout;this.tls={ca,cert,key:privateKey,rejectUnauthorized:true,minVersion:'TLSv1.2'};
 }
 async send(proof,url=this.url){
  const payload=Buffer.from(JSON.stringify(proof));
  return new Promise((resolve,reject)=>{
   const req=https.request(url,{method:'POST',...this.tls,headers:{'content-type':'application/json','content-length':payload.length}},res=>{
    if(res.statusCode!==200){res.resume();reject(Error('PRIVATE_SOURCE_UNAVAILABLE'));return;}
    const chunks=[];let bytes=0;res.on('data',chunk=>{bytes+=chunk.length;if(bytes>16384){res.destroy();reject(Error('PRIVATE_SOURCE_RESPONSE_SIZE'));}else chunks.push(chunk);});
    res.on('end',()=>{try{resolve(JSON.parse(Buffer.concat(chunks)));}catch{reject(Error('PRIVATE_SOURCE_RESPONSE'));}});res.on('error',()=>reject(Error('PRIVATE_SOURCE_UNAVAILABLE')));
   });
   const timer=setTimeout(()=>req.destroy(),this.timeout);req.on('close',()=>clearTimeout(timer));req.on('error',()=>reject(Error('PRIVATE_SOURCE_UNAVAILABLE')));req.end(payload);
  });
 }
 async exchange(request){
  return verifyResponse(await this.send(signRequest(request,this.key)),request,this.key);
 }
 async own(request){
  const url=new URL(this.url);url.pathname='/internal/private-events/ownership';
  return verifyOwnershipResponse(await this.send(signOwnership(request,this.key),url),request,this.key);
 }
}
/** All retry ownership is a durable Kafka work record. No database retry loop,
 * attempt log or in-memory timer is an authority. Source receipts fence egress. */
export class DeliveryCoordinator {
 constructor({client,now=()=>Math.floor(Date.now()/1000),delays=[30,300,3600],probeDelay=15,maxProbes=8}){
  if(!Array.isArray(delays)||delays.length!==3||!delays.every(n=>integer(n,1,86400))||!integer(probeDelay,1,300)||!integer(maxProbes,1,32))fail();
  Object.assign(this,{client,now,delays,probeDelay,maxProbes});
 }
 request(item,lease){return {version:1,...ordered(item.ref,refs),lease_generation:lease,request_id:requestId(item.ref,lease),issued_at:this.now()};}
 retry(item,response,probe=false){
  if(probe){if(item.probes>=this.maxProbes)return {kind:'dlq',code:'SOURCE_OUTCOME_UNRESOLVED',response};return {kind:'retry',next:{...item,probes:item.probes+1,available_at:this.now()+this.probeDelay},level:Math.max(1,Math.min(item.attempt+1,3)),response};}
  if(item.attempt>=item.retry_budget)return {kind:'dlq',code:'RETRY_LIMIT',response};
  return {kind:'retry',next:{...item,lease_generation:item.lease_generation+1,attempt:item.attempt+1,probes:0,available_at:this.now()+this.delays[item.attempt]},level:item.attempt+1,response};
 }
 outcome(item,r){
  if(r.status==='DELIVERED'||['DISABLED','NO_DESTINATION'].includes(r.status))return {kind:'complete',response:r};
  if(['PERMANENT','UNCERTAIN'].includes(r.status))return {kind:'dlq',code:r.status,response:r};
  if(r.lease_generation>item.lease_generation)return {kind:'stale',response:r};
  if(r.lease_generation===item.lease_generation&&r.status==='RETRYABLE')return this.retry(item,r);
  return this.retry(item,r,true);
 }
 async run(item){
  exact(item,['version','ref','base_lease','retry_budget','lease_generation','attempt','probes','available_at']);validReference(item.ref);
  if(item.version!==1||!integer(item.base_lease,0,Number.MAX_SAFE_INTEGER-4)||!integer(item.retry_budget,0,3)||!integer(item.lease_generation,1)||!integer(item.attempt,0,item.retry_budget)||!integer(item.probes,0,32)||!integer(item.available_at)||item.lease_generation!==item.base_lease+item.attempt+1)fail();
  if(item.available_at>this.now())return {kind:'wait',waitMs:Math.min(60000,(item.available_at-this.now())*1000)};
  try{
   let r=await this.client.exchange(this.request(item,0));
   if(['PENDING','RETRYABLE'].includes(r.status)&&r.lease_generation===item.lease_generation-1)r=await this.client.exchange(this.request(item,item.lease_generation));
   return this.outcome(item,r);
  }catch{return this.retry(item,null,true);}
 }
}
