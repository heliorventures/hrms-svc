// Offline deployment validation. Reads public keys only; emits no key material.
import { readFileSync } from 'node:fs';
import { createPublicKey, createHash } from 'node:crypto';
import { isAbsolute } from 'node:path';
import { pathToFileURL } from 'node:url';
const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
export function validateKeys(manifest,read=readFileSync){
 if(!manifest||!Array.isArray(manifest.keys)||!manifest.keys.length||manifest.keys.length>500)throw Error('KEY_MANIFEST_INVALID');
 const ids=new Set(),identities=new Map();
 for(const entry of manifest.keys){
  if(!['client','service','delegation'].includes(entry.purpose)||!Array.isArray(entry.tenant_ids)||!entry.tenant_ids.length||entry.tenant_ids.some(t=>!uuid.test(t))||new Set(entry.tenant_ids).size!==entry.tenant_ids.length||typeof entry.kid!=='string'||!/^[A-Za-z0-9_.-]{1,100}$/.test(entry.kid)||!isAbsolute(entry.public_key_file||''))throw Error('KEY_ENTRY_INVALID');
  const service=entry.purpose==='service'?entry.service:entry.purpose;
  if(typeof service!=='string'||!/^[a-z][a-z0-9-]{1,80}$/.test(service))throw Error('KEY_SERVICE_INVALID');
  if((entry.purpose==='client'||entry.purpose==='service'&&service!=='kabipay-organization')&&entry.tenant_ids.length!==1)throw Error('KEY_COMPANY_BINDING_INVALID');
  const id=entry.purpose+':'+entry.kid;if(ids.has(id))throw Error('KEY_ID_DUPLICATE');ids.add(id);
  const pem=read(entry.public_key_file);if(pem.length>16384||pem.toString().includes('PRIVATE KEY'))throw Error('PUBLIC_KEY_REQUIRED');
  const key=createPublicKey(pem);if(key.asymmetricKeyType!=='rsa'||key.asymmetricKeyDetails.modulusLength<2048)throw Error('RSA_2048_REQUIRED');
  const fingerprint=createHash('sha256').update(key.export({type:'spki',format:'der'})).digest('hex');
  const identity=entry.purpose+':'+service+':'+[...entry.tenant_ids].sort().join(',');
  if(identities.has(fingerprint)&&identities.get(fingerprint)!==identity)throw Error('KEY_REUSED_ACROSS_TRUST_BOUNDARIES');
  identities.set(fingerprint,identity);
 }
 return {keys:ids.size,distinctPublicKeys:identities.size};
}
if(process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href){
 try{const path=process.argv[2];if(!path)throw Error('PUBLIC_KEY_MANIFEST_PATH_REQUIRED');const result=validateKeys(JSON.parse(readFileSync(path,'utf8')));console.log(JSON.stringify({valid:true,...result}));}catch(error){console.error(error.message);process.exitCode=1;}
}
