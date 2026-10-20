import {randomUUID} from 'node:crypto';
import {timestampNanos,inExactRange} from './time.mjs';
const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
function required(ok,code='ARCHIVE_REQUEST_DENIED'){if(!ok)throw Error(code);}
function fields(o,keys){required(o&&typeof o==='object'&&!Array.isArray(o)&&Object.keys(o).length===keys.length&&keys.every(k=>Object.hasOwn(o,k)));}
function parseRequest(body,route){
 const r=JSON.parse(body);fields(r,['archive_id','organization_id','source_tenant_id','membership_id','membership_version','actor',...(route==='/v1/archive/export'?['from','through']:[])]);
 for(const k of ['archive_id','organization_id','source_tenant_id','membership_id'])required(typeof r[k]==='string'&&uuid.test(r[k]));required(Number.isSafeInteger(r.membership_version)&&r.membership_version>0);
 fields(r.actor,['principal_id','source_tenant_id','source_user_id']);for(const v of Object.values(r.actor))required(typeof v==='string'&&uuid.test(v));
 if(route==='/v1/archive/export')required(timestampNanos(r.from)<=timestampNanos(r.through));return r;
}
export function archiveHandler({proof,pool,history,archive,bindings,audit=()=>{}}){
 let active=0;
 return async function handle(req,res){
  const route=req.url;let r,client,held=false,manifest;
  try{
   required(req.method==='POST'&&['/v1/archive/export','/v1/archive/retrieve'].includes(route));required(req.headers['content-type']==='application/json');
   let bytes=0;const parts=[];for await(const chunk of req){bytes+=chunk.length;required(bytes<=131072,'ARCHIVE_REQUEST_TOO_LARGE');parts.push(chunk);}const body=Buffer.concat(parts);r=parseRequest(body,route);
   required(bindings[r.source_tenant_id]?.includes(r.organization_id));proof.verify(req.headers.authorization?.replace(/^Bearer /,''),{tenant:r.source_tenant_id,route,body});
   required(active<2,'ARCHIVE_BUSY');active++;held=true;client=await pool.connect();await client.query('BEGIN');await client.query('SELECT pg_advisory_xact_lock(hashtextextended($1,0))',['archive:'+r.archive_id]);
   const scope={organization_id:r.organization_id,source_tenant_id:r.source_tenant_id,membership_id:r.membership_id,membership_version:r.membership_version};
   const authorize=async()=>{
    // Root enforces current exact end-user authority before and after this RPC.
    // Retrieval uses the immutable archive epoch; a safe resume must not make
    // its old authenticated bytes inaccessible to currently authorized users.
    if(route==='/v1/archive/retrieve')return Boolean((await client.query('SELECT kabipay_ops.organization_archive_member_recognized($1,$2,$3) AS recognized',[r.organization_id,r.source_tenant_id,r.membership_id])).rows[0]?.recognized);
    const result=await client.query(`SELECT c.membership_id,c.membership_version,to_char(c.history_from AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS history_from,kabipay_ops.organization_capture_member_classification($1,$2) AS classification,kabipay_ops.organization_capture_healthy($1,$2,ARRAY['inbox','history','reporting']) AS healthy,(SELECT to_char(m.source_marker_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"') FROM kabipay_ops.organization_capture_marker m WHERE m.organization_id=c.organization_id AND m.source_tenant_id=c.source_tenant_id AND m.topic=c.topic ORDER BY m.source_marker_at DESC,m.marker_id DESC LIMIT 1) AS source_marker_at FROM kabipay_ops.organization_company_capture c WHERE c.organization_id=$1 AND c.source_tenant_id=$2 AND c.acceptance_status='ACCEPTED'`,[r.organization_id,r.source_tenant_id]);
    const row=result.rows[0];return Boolean(row&&row.membership_id===r.membership_id&&Number(row.membership_version)===r.membership_version&&row.classification==='ACTIVE'&&row.healthy&&row.history_from&&row.source_marker_at&&timestampNanos(row.history_from)<=timestampNanos(r.from)&&timestampNanos(r.through)<=timestampNanos(row.source_marker_at));
   };
   required(await authorize(),'ARCHIVE_MEMBERSHIP_DENIED');
   if(route==='/v1/archive/export'){
    const existing=(await client.query("SELECT manifest FROM kabipay_ops.organization_archive_manifest WHERE id=$1 AND organization_id=$2 AND source_tenant_id=$3 AND state='VERIFIED'",[r.archive_id,r.organization_id,r.source_tenant_id])).rows[0];
    if(existing){manifest=existing.manifest;required(timestampNanos(manifest.history_from)===timestampNanos(r.from)&&timestampNanos(manifest.history_through)===timestampNanos(r.through),'ARCHIVE_ID_CONFLICT');}
    else{
     manifest=await archive.recover({id:r.archive_id,scope,from:r.from,through:r.through,authorize});
     if(!manifest){
      const raw=await history.query(`SELECT source_event FROM organization.business_history FINAL WHERE organization_id={org:UUID} AND source_tenant_id={tenant:UUID} AND occurred_at>=parseDateTime64BestEffort({from:String},3,'UTC') AND occurred_at<=parseDateTime64BestEffort({through:String},3,'UTC') ORDER BY occurred_at,event_id LIMIT 10001 FORMAT JSONEachRow`,{org:r.organization_id,tenant:r.source_tenant_id,from:r.from,through:r.through});
      const rows=raw.trim().split('\n').filter(Boolean).map(line=>JSON.parse(line));required(rows.length<=10000,'ARCHIVE_EXPORT_TOO_LARGE');required(rows.every(row=>typeof row.source_event==='string'&&row.source_event.length>0),'ARCHIVE_SOURCE_ENVELOPE_UNAVAILABLE');
      const events=rows.map(row=>JSON.parse(row.source_event)).filter(e=>inExactRange(e.occurred_at,r.from,r.through));
      manifest=await archive.write({id:r.archive_id,scope,events,from:r.from,through:r.through,authorize});
     }
     required(await authorize(),'ARCHIVE_MEMBERSHIP_DENIED');
     await client.query(`INSERT INTO kabipay_ops.organization_archive_manifest(id,organization_id,source_tenant_id,membership_id,membership_version,history_from,history_through,row_count,plaintext_sha256,ciphertext_sha256,key_version,manifest,state,verified_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,'VERIFIED',clock_timestamp())`,[manifest.id,r.organization_id,r.source_tenant_id,r.membership_id,r.membership_version,r.from,r.through,manifest.row_count,manifest.plaintext_sha256,manifest.ciphertext_sha256,manifest.key_version,manifest]);
    }
   }else{manifest=(await client.query("SELECT manifest FROM kabipay_ops.organization_archive_manifest WHERE id=$1 AND organization_id=$2 AND source_tenant_id=$3 AND state='VERIFIED'",[r.archive_id,r.organization_id,r.source_tenant_id])).rows[0]?.manifest;required(manifest,'ARCHIVE_NOT_AVAILABLE');}
   const output=await archive.retrieve({manifest,scope,authorize});required(await authorize(),'ARCHIVE_MEMBERSHIP_DENIED');await audit({id:randomUUID(),operation:route.endsWith('export')?'EXPORT':'RETRIEVE',archive_id:r.archive_id,...scope,actor:r.actor,row_count:manifest.row_count},client);await client.query('COMMIT');
   res.writeHead(200,{'content-type':'application/json','cache-control':'no-store','x-content-type-options':'nosniff'});res.end(JSON.stringify(output));
  }catch(error){if(client)await client.query('ROLLBACK').catch(()=>{});res.writeHead(403,{'content-type':'application/json','cache-control':'no-store'});res.end(JSON.stringify({code:'ARCHIVE_UNAVAILABLE'}));}
  finally{client?.release();if(held)active--;}
 };
}
