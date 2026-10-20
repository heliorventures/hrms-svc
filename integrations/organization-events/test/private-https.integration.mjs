// Opt-in: node test/private-https.integration.mjs --docker-fixture
// Real loopback mTLS and production Node transports/validators. The source and
// coverage store below are fixtures, not Rust/PostgreSQL/Kafka/receiver proof.
import test from 'node:test';
import assert from 'node:assert/strict';
import {randomBytes,randomUUID,createHash} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import {mkdtemp,readFile,writeFile,realpath,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import https from 'node:https';
import {once} from 'node:events';
import {SourceDeliveryClient,DeliveryCoordinator,newWork,signRequest,signResponse} from '../src/private/delivery.mjs';
import {signOwnership,signOwnershipResponse,ownershipHash} from '../src/private/ownership.mjs';
import {PrivateHistoryQuery,privateHistoryHandler,encodeProof,decodeProof} from '../src/private/query.mjs';

const enabled=process.argv.includes('--docker-fixture');
// Cached Debian image supplies OpenSSL only; its database entrypoint is bypassed.
const fixtureImage='postgres:16';
const pkiScript=`#!/bin/sh
set -eu
umask 077
cd /pki
for name in server-ca client-ca rogue-ca; do
 openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 1 -subj "/CN=synthetic-$name" -keyout "$name.key" -out "$name.crt" >/dev/null 2>&1
done
issue() {
 name="$1"; issuer="$2"; usage="$3"; san="$4"
 openssl req -new -newkey rsa:2048 -nodes -sha256 -subj "/CN=synthetic-$name" -keyout "$name.key" -out "$name.csr" >/dev/null 2>&1
 printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=%s\nsubjectAltName=%s\n' "$usage" "$san" > "$name.ext"
 openssl x509 -req -in "$name.csr" -CA "$issuer.crt" -CAkey "$issuer.key" -CAcreateserial -days 1 -sha256 -extfile "$name.ext" -out "$name.crt" >/dev/null 2>&1
}
issue server server-ca serverAuth IP:127.0.0.1
issue wrong-host server-ca serverAuth DNS:wrong-host.invalid
issue client client-ca clientAuth DNS:synthetic-source-client.invalid
issue rogue-client rogue-ca clientAuth DNS:synthetic-rogue-client.invalid
`;

function docker(args){
 const result=spawnSync('docker',args,{encoding:'utf8',timeout:90000,windowsHide:true});
 assert.ifError(result.error);assert.equal(result.status,0,'Synthetic Docker PKI command failed; no remote service was contacted');
}
async function startServer(options,handler,servers){
 const server=https.createServer({...options,requestCert:true,rejectUnauthorized:true,minVersion:'TLSv1.2'},handler);
 const sockets=new Set();server.on('connection',socket=>{sockets.add(socket);socket.once('close',()=>sockets.delete(socket));});
 server.on('tlsClientError',()=>{});server.requestTimeout=5000;server.headersTimeout=5000;
 servers.push({server,sockets});server.listen(0,'127.0.0.1');await once(server,'listening');
 return `https://127.0.0.1:${server.address().port}`;
}
async function body(request){
 const chunks=[];let size=0;
 for await(const chunk of request){size+=chunk.length;if(size>24000)throw Error('FIXTURE_REQUEST_SIZE');chunks.push(chunk);}
 return JSON.parse(Buffer.concat(chunks));
}
function json(response,status,value){response.writeHead(status,{'content-type':'application/json','cache-control':'no-store'});response.end(JSON.stringify(value));}
function request(url,options,payload,method='POST'){
 const bytes=Buffer.from(JSON.stringify(payload));
 return new Promise((resolve,reject)=>{
  const req=https.request(url,{...options,method,agent:false,headers:{'content-type':'application/json','content-length':bytes.length}},res=>{
   const chunks=[];let size=0;res.on('data',chunk=>{size+=chunk.length;if(size>1024*1024){res.destroy();return;}chunks.push(chunk);});
   res.on('error',reject);res.on('end',()=>{try{resolve({status:res.statusCode,headers:res.headers,body:chunks.length?JSON.parse(Buffer.concat(chunks)):null});}catch(error){reject(error);}});
  });
  req.setTimeout(5000,()=>req.destroy(Error('FIXTURE_TIMEOUT')));req.on('error',reject);req.end(bytes);
 });
}

test('private delivery, ownership and history use verified HTTPS/mTLS without URL or trust bypass', {skip:!enabled,timeout:180000},async t=>{
 const folder=await mkdtemp(path.join(tmpdir(),'kabipay-private-https-'));
 const container=`kabipay-private-pki-${randomUUID()}`;const servers=[];
 try{
  await writeFile(path.join(folder,'generate.sh'),pkiScript,{mode:0o600});
  docker(['image','inspect',fixtureImage]); // Cached only: run cannot pull or reach a network.
  docker(['run','--rm','--pull=never','--name',container,'--network','none','--read-only','--tmpfs','/tmp','--mount',`type=bind,source=${folder},target=/pki`,'--entrypoint','sh',fixtureImage,'/pki/generate.sh']);
  const pem=async name=>readFile(path.join(folder,name));
  const [serverKey,serverCert,serverCa,clientCa,clientKey,clientCert,rogueCa,rogueKey,rogueCert,wrongKey,wrongCert]=await Promise.all(['server.key','server.crt','server-ca.crt','client-ca.crt','client.key','client.crt','rogue-ca.crt','rogue-client.key','rogue-client.crt','wrong-host.key','wrong-host.crt'].map(pem));
  const tls={key:serverKey,cert:serverCert,ca:clientCa};
  const transport={ca:serverCa,cert:clientCert,key:clientKey,rejectUnauthorized:true,minVersion:'TLSv1.2'};
  const tenant=randomUUID(),key=randomBytes(32),historyKey=randomBytes(32),owner=randomUUID();
  const states=new Map(),ownership=new Map();let handlerCalls=0,acceptedOwnership=0;
  const reference=()=>({tenant_id:tenant,intent_id:randomUUID(),destination_id:randomUUID(),source_generation:1,destination_version:1,intent_hash:randomBytes(32).toString('hex')});
  function state(ref,settings={}){const value={ref,lease:0,status:'PENDING',sends:0,requests:[],ack:randomUUID(),...settings};states.set(ref.intent_id,value);return value;}
  function checkScope(r){const value=states.get(r.intent_id);assert(value);for(const [name,expected] of Object.entries(value.ref))assert.equal(r[name],expected);assert(r.issued_at>=Math.floor(Date.now()/1000)-60);return value;}
  const origin=await startServer(tls,async(req,res)=>{
   handlerCalls++;
   if(req.method!=='POST'||!['/internal/private-events/delivery','/internal/private-events/ownership'].includes(req.url)){res.writeHead(404);res.end();return;}
   try{
    const proof=await body(req),r=proof.request,value=checkScope(r);
    if(req.url.endsWith('/ownership')){
     assert.deepEqual(proof,signOwnership(r,key));assert.equal(r.broker_owner_id,owner);
     const id=`${r.intent_id}/${r.destination_id}`,previous=ownership.get(id),hash=previous??ownershipHash(r);ownership.set(id,hash);acceptedOwnership++;
     json(res,200,signOwnershipResponse({...r,receipt_sha256:hash,accepted_at:Math.floor(Date.now()/1000),already_owned:previous!==undefined},key));return;
    }
    assert.deepEqual(proof,signRequest(r,key));value.requests.push({...r});
    if(r.lease_generation>0){
     assert(['PENDING','RETRYABLE'].includes(value.status));assert.equal(r.lease_generation,value.lease+1);
     value.lease=r.lease_generation;value.sends++;value.status=value.outcomes?.[value.lease]??'DELIVERED';
     if(value.dropNext){value.dropNext=false;req.socket.destroy();return;}
    }
    const response={...r,lease_generation:value.lease,status:value.status,outcome_code:'synthetic',acknowledgement_id:value.status==='DELIVERED'?value.ack:null,...value.responseMutation};
    json(res,200,signResponse(response,key));
   }catch{json(res,403,{error:'FIXTURE_PROOF_DENIED'});}
  },servers);
  const clientOptions={url:`${origin}/internal/private-events/delivery`,key,ca:serverCa,cert:clientCert,privateKey:clientKey,timeout:5000};
  const client=new SourceDeliveryClient(clientOptions);
  const deliveryRequest=ref=>({version:1,...ref,lease_generation:0,request_id:randomUUID(),issued_at:Math.floor(Date.now()/1000)});

  await t.test('TLS rejects absent/foreign client certificates, foreign server CA and wrong server hostname before the handler',async()=>{
   const ref=reference();state(ref);const r=deliveryRequest(ref);
   for(const options of [{cert:undefined,privateKey:undefined},{cert:rogueCert,privateKey:rogueKey},{ca:rogueCa}]){
    const before=handlerCalls;await assert.rejects(new SourceDeliveryClient({...clientOptions,...options}).exchange(r),/PRIVATE_SOURCE_UNAVAILABLE/);assert.equal(handlerCalls,before);
   }
   let wrongHostCalls=0;
   const wrongOrigin=await startServer({...tls,key:wrongKey,cert:wrongCert},(_req,res)=>{wrongHostCalls++;res.end();},servers);
   await assert.rejects(new SourceDeliveryClient({...clientOptions,url:`${wrongOrigin}/internal/private-events/delivery`}).exchange(r),/PRIVATE_SOURCE_UNAVAILABLE/);assert.equal(wrongHostCalls,0);
   assert.equal((await client.exchange(r)).status,'PENDING');
  });

  await t.test('signed source references/status and fixed routes are checked over actual HTTPS',async()=>{
   const ref=reference(),value=state(ref),r=deliveryRequest(ref);
   assert.equal((await client.exchange(r)).tenant_id,tenant);
   await assert.rejects(new SourceDeliveryClient({...clientOptions,key:randomBytes(32)}).exchange(r),/PRIVATE_SOURCE_UNAVAILABLE/);
   await assert.rejects(client.exchange({...r,tenant_id:randomUUID()}),/PRIVATE_SOURCE_UNAVAILABLE/);
   for(const mutation of [{tenant_id:randomUUID()},{intent_hash:'b'.repeat(64)},{status:'NOT_A_STATUS'},{status:'DELIVERED',acknowledgement_id:null}]){
    value.responseMutation=mutation;await assert.rejects(client.exchange({...r,request_id:randomUUID()}),/PRIVATE_DELIVERY_INVALID/);
   }
   delete value.responseMutation;
   assert.equal((await request(`${origin}/internal/private-events/other`,transport,{})).status,404);
   assert.throws(()=>new SourceDeliveryClient({...clientOptions,url:`${origin}/other`}),/PRIVATE_DELIVERY_INVALID/);
   assert.throws(()=>new SourceDeliveryClient({...clientOptions,url:clientOptions.url.replace('https:','http:')}),/PRIVATE_DELIVERY_INVALID/);
  });

  await t.test('lost transport acknowledgement probes the same lease without another fixture receiver send',async()=>{
   const ref=reference(),value=state(ref,{dropNext:true});let now=Math.floor(Date.now()/1000);
   const coordinator=new DeliveryCoordinator({client,now:()=>now,probeDelay:1,delays:[1,1,1]}),work=newWork(ref,now);
   const retry=await coordinator.run(work);assert.equal(retry.kind,'retry');assert.equal(retry.next.lease_generation,work.lease_generation);assert.equal(retry.next.attempt,0);assert.equal(value.sends,1);
   assert.equal((await coordinator.run(retry.next)).kind,'wait');now=retry.next.available_at;
   assert.equal((await coordinator.run(retry.next)).kind,'complete');assert.equal((await coordinator.run(work)).kind,'complete');assert.equal(value.sends,1);
   assert.deepEqual(value.requests.map(r=>r.lease_generation),[0,1,0,0]);
   const uncertainRef=reference(),uncertain=state(uncertainRef,{dropNext:true,outcomes:{1:'UNCERTAIN'}});
   const first=await coordinator.run(newWork(uncertainRef,now));assert.equal(first.kind,'retry');now=first.next.available_at;
   const final=await coordinator.run(first.next);assert.equal(final.kind,'dlq');assert.equal(final.code,'UNCERTAIN');assert.equal(uncertain.sends,1);
   const safeRef=reference(),safe=state(safeRef,{outcomes:{1:'RETRYABLE',2:'DELIVERED'}});
   const next=await coordinator.run(newWork(safeRef,now));assert.equal(next.kind,'retry');assert.equal(next.next.lease_generation,2);assert.equal(next.next.attempt,1);now=next.next.available_at;
   assert.equal((await coordinator.run(next.next)).kind,'complete');assert.equal(safe.sends,2);
  });

  await t.test('ownership authenticates exact large broker offsets and repeated publication preserves its first receipt',async()=>{
   const ref=reference();state(ref);
   const r={version:1,...ref,broker_owner_id:owner,topic:`organization.delivery.work.${tenant}`,partition_id:0,kafka_offset:'9223372036854775800',request_id:randomUUID(),issued_at:Math.floor(Date.now()/1000)};
   const first=await client.own(r);assert.equal(first.kafka_offset,r.kafka_offset);assert.equal(first.receipt_sha256,ownershipHash(r));assert.equal(first.already_owned,false);
   const second=await client.own({...r,kafka_offset:'9223372036854775801',request_id:randomUUID()});assert.equal(second.already_owned,true);assert.equal(second.receipt_sha256,first.receipt_sha256);assert.equal(acceptedOwnership,2);
   await assert.rejects(client.own({...r,kafka_offset:'9223372036854775808'}),/PRIVATE_OWNERSHIP_INVALID/);
   assert.equal(acceptedOwnership,2);
  });

  await t.test('production history handler enforces mTLS, purpose/scope proofs, replay, coverage and private route',async()=>{
   const coverage=randomUUID(),hash='c'.repeat(64),raw=JSON.stringify({id:randomUUID(),tenant_id:tenant,entity_type:'unknown_legacy',action:'custom.action',payload:'हिस्ट्री / exact bytes'});
   let historyCalls=0,reads=0,covered=0;
   const query=new PrivateHistoryQuery({bindings:[{tenant_id:tenant,source_generation:1,database:'synthetic_source',schema:'company_schema',key_b64:historyKey.toString('base64')}],store:{
    async requireCoverage(r){covered++;assert.equal(r.source_database,'synthetic_source');assert.equal(r.source_schema,'company_schema');if(r.coverage_id!==coverage||r.coverage_sha256!==hash)throw Error('FIXTURE_COVERAGE_MISSING');},
    async readAudit(filter){reads++;assert.equal(filter.tenant_id,tenant);return {records:[raw],total:1};},
   }});
   const handle=privateHistoryHandler(query),historyOrigin=await startServer(tls,(req,res)=>{historyCalls++;return handle(req,res);},servers);
   const url=`${historyOrigin}/internal/private-events/history`;
   const historyRequest=()=>({version:1,tenant_id:tenant,source_generation:1,request_id:randomUUID(),issued_at:Math.floor(Date.now()/1000),filter:{tenant_id:tenant,entity_type:null,entity_id:null,actions:[],since:null,until:null,offset:0,limit:10},coverage_id:coverage,coverage_sha256:hash});
   const r=historyRequest(),proof=encodeProof(r,historyKey,'request'),success=await request(url,transport,proof);
   assert.equal(success.status,200);assert.equal(success.headers['cache-control'],'no-store');
   const response=decodeProof(success.body,historyKey,'response');assert.equal(response.request_id,r.request_id);assert.equal(response.request_hash,createHash('sha256').update(JSON.stringify(r)).digest('hex'));assert.equal(response.page.records[0],raw);
   const beforeReads=reads,beforeCoverage=covered;
   for(const bad of [proof,encodeProof({...historyRequest(),tenant_id:randomUUID()},historyKey,'request'),encodeProof({...historyRequest(),source_generation:2},historyKey,'request'),encodeProof(historyRequest(),randomBytes(32),'request'),encodeProof(historyRequest(),historyKey,'request','delivery-history')])assert.equal((await request(url,transport,bad)).status,403);
   assert.equal(reads,beforeReads);assert.equal(covered,beforeCoverage);
   assert.equal((await request(url,transport,encodeProof({...historyRequest(),coverage_id:randomUUID()},historyKey,'request'))).status,503);assert.equal(reads,beforeReads);
   assert.equal((await request(`${historyOrigin}/internal/private-events/other`,transport,proof)).status,404);assert.equal((await request(url,transport,proof,'GET')).status,404);
   for(const options of [{cert:undefined,key:undefined},{cert:rogueCert,key:rogueKey}]){const before=historyCalls;await assert.rejects(request(url,{...transport,...options},encodeProof(historyRequest(),historyKey,'request')));assert.equal(historyCalls,before);}
  });
 }finally{
  for(const {server,sockets} of servers){const closed=new Promise(resolve=>server.close(resolve));for(const socket of sockets)socket.destroy();await closed;}
  // Only this randomly named fixture container and direct temp child are eligible.
  assert.match(container,/^kabipay-private-pki-[0-9a-f-]{36}$/);
  spawnSync('docker',['rm','--force',container],{encoding:'utf8',timeout:15000,windowsHide:true});
  const resolved=await realpath(folder);assert.equal(path.dirname(resolved),await realpath(tmpdir()));assert.match(path.basename(resolved),/^kabipay-private-https-/);await rm(resolved,{recursive:true,force:true});
 }
});
