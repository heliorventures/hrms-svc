import {createCipheriv,createDecipheriv,randomBytes,timingSafeEqual} from 'node:crypto';
import {readFile,open,mkdir,lstat,link,unlink} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {parseEvent,stable,digest} from './events.mjs';
import {inExactRange,timestampNanos} from './time.mjs';
const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
function requireValue(ok,code='INVALID_ARCHIVE'){if(!ok)throw Error(code);}
function scopeEquals(a,b){return ['organization_id','source_tenant_id','membership_id','membership_version'].every(k=>a[k]===b[k]);}
function scopeValid(s){return s&&['organization_id','source_tenant_id','membership_id'].every(k=>typeof s[k]==='string'&&uuid.test(s[k]))&&Number.isSafeInteger(s.membership_version)&&s.membership_version>0;}
async function authorized(authorize,scope){requireValue(typeof authorize==='function'&&await authorize(scope)===true,'ARCHIVE_ACCESS_DENIED');}
export class EncryptedArchive {
 constructor({root,keyVersion,key,readKeys={}}){requireValue(typeof root==='string'&&root.length>0&&typeof keyVersion==='string'&&/^[a-zA-Z0-9._-]{1,80}$/.test(keyVersion)&&Buffer.isBuffer(key)&&key.length===32);this.root=resolve(root);this.keyVersion=keyVersion;this.key=Buffer.from(key);this.keys=new Map([[keyVersion,this.key]]);for(const [version,material]of Object.entries(readKeys)){requireValue(/^[a-zA-Z0-9._-]{1,80}$/.test(version)&&Buffer.isBuffer(material)&&material.length===32&&!this.keys.has(version));this.keys.set(version,Buffer.from(material));}}
 path(id){requireValue(typeof id==='string'&&uuid.test(id));return join(this.root,id+'.archive');}
 validate(events,scope,from,through){
  requireValue(scopeValid(scope)&&Array.isArray(events)&&events.length<=10000&&timestampNanos(from)<=timestampNanos(through));
  const binding={archive:{organization_id:scope.organization_id,tenant_id:scope.source_tenant_id}};const ids=new Set();const versions=new Map();
  for(const e of events){const parsed=parseEvent(Buffer.from(stable(e)),'archive',binding);requireValue(!ids.has(e.event_id),'ARCHIVE_DUPLICATE_ID');ids.add(e.event_id);requireValue(inExactRange(e.occurred_at,from,through),'ARCHIVE_RANGE');const k=[e.aggregate_type,e.aggregate_id,e.event_type,e.aggregate_version].join(':');requireValue(!versions.has(k)||versions.get(k)===parsed.payload_hash,'ARCHIVE_VERSION_CONFLICT');versions.set(k,parsed.payload_hash);}
  return [...events].sort((a,b)=>{const x=timestampNanos(a.occurred_at),y=timestampNanos(b.occurred_at);return x<y?-1:x>y?1:a.event_id.localeCompare(b.event_id);});
 }
 async write({id,scope,events,from,through,authorize}){
  await authorized(authorize,scope);const rows=this.validate(events,scope,from,through);const plaintext=Buffer.from(stable(rows));requireValue(plaintext.length<=16*1024*1024,'ARCHIVE_TOO_LARGE');
  const manifest={schema_version:1,id,...scope,history_from:from,history_through:through,row_count:rows.length,plaintext_sha256:digest(plaintext),key_version:this.keyVersion,events:rows.map(e=>({event_id:e.event_id,sha256:digest(e)}))};
  const nonce=randomBytes(12);const cipher=createCipheriv('aes-256-gcm',this.key,nonce);cipher.setAAD(Buffer.from(stable(manifest)));const ciphertext=Buffer.concat([cipher.update(plaintext),cipher.final()]);
  const bytes=Buffer.from(stable({manifest,nonce:nonce.toString('base64'),tag:cipher.getAuthTag().toString('base64'),ciphertext:ciphertext.toString('base64')}));
  await mkdir(this.root,{recursive:true,mode:0o700});requireValue(!(await lstat(this.root)).isSymbolicLink(),'ARCHIVE_STORAGE_SYMLINK');
  const path=this.path(id),temporary=join(this.root,`.${id}.${randomBytes(12).toString('hex')}.tmp`);
  const handle=await open(temporary,'wx',0o600);try{await handle.writeFile(bytes);await handle.sync();}finally{await handle.close();}
  let existing;
  try{await authorized(authorize,scope);await link(temporary,path);}catch(error){if(error.code!=='EEXIST')throw error;const stat=await lstat(path);requireValue(stat.isFile()&&!stat.isSymbolicLink()&&stat.size<=24*1024*1024);const previous=await readFile(path),container=JSON.parse(previous);requireValue(stable(container.manifest)===stable(manifest),'ARCHIVE_ID_CONFLICT');existing={...container.manifest,ciphertext_sha256:digest(previous)};}finally{await unlink(temporary);}
  if(existing){await this.retrieve({manifest:existing,scope,authorize});return existing;}
  if(process.platform!=='win32'){const directory=await open(this.root,'r');try{await directory.sync();}finally{await directory.close();}}
  const result={...manifest,ciphertext_sha256:digest(bytes)};
  // Registration is permitted only after this full decrypt+hash verification.
  await this.retrieve({manifest:result,scope,authorize});return result;
 }
 async retrieve({manifest,scope,authorize}){
  await authorized(authorize,scope);requireValue(scopeValid(scope)&&scopeEquals(manifest,scope),'ARCHIVE_SCOPE');const decryptionKey=this.keys.get(manifest.key_version);requireValue(decryptionKey,'ARCHIVE_KEY_VERSION');
  const path=this.path(manifest.id);const stat=await lstat(path);requireValue(stat.isFile()&&!stat.isSymbolicLink()&&stat.size<=24*1024*1024,'ARCHIVE_STORAGE');const bytes=await readFile(path);
  requireValue(typeof manifest.ciphertext_sha256==='string'&&/^[a-f0-9]{64}$/.test(manifest.ciphertext_sha256)&&timingSafeEqual(Buffer.from(digest(bytes),'hex'),Buffer.from(manifest.ciphertext_sha256,'hex')),'ARCHIVE_CIPHERTEXT_HASH');
  const container=JSON.parse(bytes);const {ciphertext_sha256,...expected}=manifest;requireValue(stable(container.manifest)===stable(expected),'ARCHIVE_MANIFEST_CONFLICT');
  const nonce=Buffer.from(container.nonce,'base64'),tag=Buffer.from(container.tag,'base64');requireValue(nonce.length===12&&tag.length===16);
  const cipher=createDecipheriv('aes-256-gcm',decryptionKey,nonce);cipher.setAAD(Buffer.from(stable(expected)));cipher.setAuthTag(tag);const plaintext=Buffer.concat([cipher.update(Buffer.from(container.ciphertext,'base64')),cipher.final()]);
  requireValue(digest(plaintext)===manifest.plaintext_sha256,'ARCHIVE_PLAINTEXT_HASH');const events=this.validate(JSON.parse(plaintext),scope,manifest.history_from,manifest.history_through);
  requireValue(events.length===manifest.row_count&&stable(events.map(e=>({event_id:e.event_id,sha256:digest(e)})))===stable(manifest.events),'ARCHIVE_RECONCILIATION');
  await authorized(authorize,scope);return {manifest,events};
 }
 async recover({id,scope,from,through,authorize}){
  await authorized(authorize,scope);const path=this.path(id);let stat;try{stat=await lstat(path);}catch(error){if(error.code==='ENOENT')return null;throw error;}requireValue(stat.isFile()&&!stat.isSymbolicLink()&&stat.size<=24*1024*1024,'ARCHIVE_STORAGE');
  const bytes=await readFile(path),container=JSON.parse(bytes),manifest={...container.manifest,ciphertext_sha256:digest(bytes)};
  requireValue(timestampNanos(manifest.history_from)===timestampNanos(from)&&timestampNanos(manifest.history_through)===timestampNanos(through),'ARCHIVE_ID_CONFLICT');
  return (await this.retrieve({manifest,scope,authorize})).manifest;
 }
}
