import test from 'node:test';
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {verifyExportChunk} from '../src/backfill.mjs';
import {digest} from '../src/events.mjs';

const id='00000000-0000-4000-8000-000000000001';
const tenant='00000000-0000-4000-8000-000000000002';
function validateCli(kind){
 const env={...process.env,SOURCE_TENANT_ID:tenant,SOURCE_ORGANIZATION_ID:id,SOURCE_EXPORT_ID:id,SOURCE_SCHEMA:'source_one',SOURCE_EXPORT_KIND:kind,SOURCE_COMPANY_TOPIC:`organization.company.${tenant}`,SOURCE_DATABASE_NAME:'synthetic'};
 // Validation must reject the retired kind before reading any credential or opening a connection.
 delete env.SOURCE_COMPANY_DATABASE_URL;
 return spawnSync(process.execPath,['src/source-export-cli.mjs'],{cwd:new URL('..',import.meta.url),env,encoding:'utf8',windowsHide:true,timeout:30000});
}
test('source CLI rejects new legacy audit exports before requesting source credentials',()=>{
 const r=validateCli('LEGACY_AUDIT');assert.equal(r.status,1);assert.match(r.stderr,/INVALID_SOURCE_EXPORT_CONFIGURATION/);assert.doesNotMatch(r.stderr,/MISSING_SOURCE_COMPANY_DATABASE_URL/);
});
test('source CLI retains canonical OUTBOX validation',()=>{
 const r=validateCli('OUTBOX');assert.equal(r.status,1);assert.match(r.stderr,/MISSING_SOURCE_COMPANY_DATABASE_URL/);assert.doesNotMatch(r.stderr,/INVALID_SOURCE_EXPORT_CONFIGURATION/);
});
test('previously stored legacy manifests retain their historical parser',()=>{
 const event={schema_version:1,event_id:id,event_type:'legacy_audit.recorded',organization_id:id,source_tenant_id:tenant,aggregate_type:'legacy_audit',aggregate_id:id,aggregate_version:1,occurred_at:'2026-10-19T00:00:00Z',deleted:false,payload:{source_audit_id:id,source_entity_type:'LEAVE_REQUEST',source_entity_id:id,source_action:'APPROVED',source_user_id:null,before_status:'PENDING',after_status:'APPROVED',source_sha256:'a'.repeat(64)}};
 const raw=JSON.stringify(event),manifest={schema_version:1,export_id:id,source_kind:'LEGACY_AUDIT',tenant_id:tenant,organization_id:id,membership_id:id,membership_version:1,chunk_index:0,after_position:0,through_position:1,cutoff_position:1,row_count:1,canonicalization:'POSTGRES_JSONB_TEXT_UTF8_V1',rows:[{source_position:1,id,raw,sha256:digest(raw)}]},rawManifest=JSON.stringify(manifest);
 const result=verifyExportChunk({rawManifest,expectedSha256:digest(rawManifest),scope:{organization_id:id,source_tenant_id:tenant,membership_id:id,membership_version:1},topic:'company',binding:{organization_id:id,tenant_id:tenant},previous:{chunk_index:-1,through_position:0}});
 assert.equal(result.events[0].event_type,'legacy_audit.recorded');assert.equal(result.events[0].payload.source_audit_id,id);
});
