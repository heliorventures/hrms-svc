import {createPublicKey,verify} from 'node:crypto';
import {digest} from './events.mjs';
const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
function requireValue(ok){if(!ok)throw Error('SERVICE_PROOF_DENIED');}
function exact(o,keys){requireValue(o&&typeof o==='object'&&!Array.isArray(o)&&Object.keys(o).length===keys.length&&keys.every(k=>Object.hasOwn(o,k)));}
export class ServiceProofVerifier {
 constructor(entries){requireValue(Array.isArray(entries)&&entries.length>0&&entries.length<=50);this.keys=new Map();const material=new Set();for(const e of entries){requireValue(e.service==='kabipay-organization'&&typeof e.kid==='string'&&/^[a-zA-Z0-9._-]{1,80}$/.test(e.kid)&&!this.keys.has(e.kid)&&Array.isArray(e.tenant_ids)&&e.tenant_ids.length>0&&new Set(e.tenant_ids).size===e.tenant_ids.length&&e.tenant_ids.every(x=>typeof x==='string'&&uuid.test(x))&&typeof e.public_pem==='string'&&!e.public_pem.includes('PRIVATE KEY')&&e.public_pem.length<=16384);const publicKey=createPublicKey(e.public_pem);requireValue(publicKey.asymmetricKeyType==='rsa'&&(publicKey.asymmetricKeyDetails?.modulusLength??0)>=2048);const identity=publicKey.export({type:'spki',format:'pem'}).toString();requireValue(!material.has(identity));material.add(identity);this.keys.set(e.kid,{key:publicKey,tenants:new Set(e.tenant_ids)});}}
 verify(token,{tenant,route,body,now=Math.floor(Date.now()/1000)}){
  requireValue(typeof token==='string'&&token.length<=16384&&Buffer.isBuffer(body)&&body.length<=131072&&uuid.test(tenant));const parts=token.split('.');requireValue(parts.length===3&&parts.every(x=>/^[a-zA-Z0-9_-]+$/.test(x)));
  const header=JSON.parse(Buffer.from(parts[0],'base64url')),c=JSON.parse(Buffer.from(parts[1],'base64url'));exact(header,['typ','alg','kid']);requireValue(header.typ==='hrms-service+jwt'&&header.alg==='RS256');const trusted=this.keys.get(header.kid);requireValue(trusted&&trusted.tenants.has(tenant));
  requireValue(verify('RSA-SHA256',Buffer.from(parts[0]+'.'+parts[1]),trusted.key,Buffer.from(parts[2],'base64url')));
  exact(c,['iss','aud','service','tenant_id','jti','iat','nbf','exp','method','route','request_hash']);
  requireValue(c.iss==='kabipay-internal-service'&&c.aud==='kabipay-organization-archive'&&c.service==='kabipay-organization'&&c.tenant_id===tenant&&c.method==='POST'&&c.route===route&&c.request_hash===digest(body)&&uuid.test(c.jti));
  requireValue([c.iat,c.nbf,c.exp].every(Number.isSafeInteger)&&c.iat<=now&&c.nbf===c.iat&&c.exp>now&&c.exp-c.iat>0&&c.exp-c.iat<=60);return c;
 }
}
