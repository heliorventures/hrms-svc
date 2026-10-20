import {createHmac,createHash,timingSafeEqual} from 'node:crypto';
import {uuid} from './contract.mjs';
const identityFields=['version','tenant_id','intent_id','destination_id','source_generation','destination_version','intent_hash','broker_owner_id','topic','partition_id','kafka_offset'];
const fields=[...identityFields,'request_id','issued_at'],responseFields=[...fields,'receipt_sha256','accepted_at','already_owned'];
const ordered=(o,keys)=>Object.fromEntries(keys.map(k=>[k,o[k]]));
const fail=()=>{throw Error('PRIVATE_OWNERSHIP_INVALID');};
function exact(o,keys){if(!o||typeof o!=='object'||Array.isArray(o)||Object.keys(o).length!==keys.length||!keys.every(k=>Object.hasOwn(o,k)))fail();}
function request(r){exact(r,fields);if(r.version!==1||!['tenant_id','intent_id','destination_id','broker_owner_id','request_id'].every(k=>uuid(r[k]))||!['source_generation','destination_version'].every(k=>Number.isSafeInteger(r[k])&&r[k]>0)||!/^[a-f0-9]{64}$/.test(r.intent_hash)||r.topic!==`organization.delivery.work.${r.tenant_id}`||r.partition_id!==0||typeof r.kafka_offset!=='string'||!/^\d{1,19}$/.test(r.kafka_offset)||BigInt(r.kafka_offset)>9223372036854775807n||!Number.isSafeInteger(r.issued_at)||r.issued_at<0)fail();}
function mac(r,key,direction,keys){return createHmac('sha256',key).update(`kabipay-private-ownership-${direction}-v1\0`).update(JSON.stringify(ordered(r,keys))).digest();}
export function ownershipHash(r){request(r);return createHash('sha256').update(JSON.stringify(ordered(r,identityFields))).digest('hex');}
export function signOwnership(r,key){request(r);return {request:ordered(r,fields),signature:mac(r,key,'request',fields).toString('base64url')};}
export function signOwnershipResponse(r,key){return {response:ordered(r,responseFields),signature:mac(r,key,'response',responseFields).toString('base64url')};}
export function verifyOwnershipResponse(signed,r,key,now=Math.floor(Date.now()/1000)){
 request(r);exact(signed,['response','signature']);const response=signed.response;exact(response,responseFields);
 if(typeof signed.signature!=='string'||!/^[A-Za-z0-9_-]{43}$/.test(signed.signature))fail();const signature=Buffer.from(signed.signature,'base64url');if(signature.toString('base64url')!==signed.signature||!timingSafeEqual(signature,mac(response,key,'response',responseFields)))fail();
 if(fields.some(k=>response[k]!==r[k])||r.issued_at<now-60||r.issued_at>now+5||typeof response.already_owned!=='boolean'||!/^[a-f0-9]{64}$/.test(response.receipt_sha256)||!Number.isSafeInteger(response.accepted_at)||response.accepted_at<0||response.accepted_at>now+5||(!response.already_owned&&response.receipt_sha256!==ownershipHash(r)))fail();
 return response;
}
