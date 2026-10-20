import {digest,stable} from './events.mjs';
import {randomUUID} from 'node:crypto';
const hash=/^[a-f0-9]{64}$/;const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
export function retentionDecision(e,{manifestSha256,now=Date.now()}){
 const missing=[];for(const key of ['accepted_at','capture_verified_at','history_verified_at','consumer_verified_at','restore_verified_at','legacy_cutover_accepted_at'])if(!e?.[key]||!Number.isFinite(Date.parse(e[key]))||Date.parse(e[key])>now)missing.push(key);
 if(!e?.expires_at||Date.parse(e.expires_at)<=now)missing.push('EXPIRED_EVIDENCE');
 if(!e?.retention_before||!Number.isFinite(Date.parse(e.retention_before))||Date.parse(e.retention_before)>now)missing.push('RETENTION_RANGE');
 if(!hash.test(e?.manifest_sha256??'')||e.manifest_sha256!==manifestSha256)missing.push('MANIFEST_HASH');if(!hash.test(e?.restore_sha256??''))missing.push('RESTORE_HASH');
 return {allowed:missing.length===0,missing};
}
// This runs at a fixed source deployment with operator configuration, never a
// browser-selected pool. Production execution requires a separately accepted
// persisted policy and explicit trusted enable flag. Tests add a fixture gate.
export async function cleanupOutbox({sourcePool,evidencePool,evidenceId,scope,candidates,manifestSha256,execute=false,fixtureNonce,executionPolicy,verifyControlCoverage}){
 const now=Date.now();
 if(!uuid.test(evidenceId)||!['company','control'].includes(scope.kind)||!Array.isArray(candidates)||candidates.length<1||candidates.length>100||candidates.some(c=>!uuid.test(c.id)||!hash.test(c.sha256))||new Set(candidates.map(c=>c.id)).size!==candidates.length||!hash.test(manifestSha256))throw Error('INVALID_RETENTION_REQUEST');
 const schema=scope.kind==='control'?'kabipay_ops':scope.schema;if(!/^[a-z_][a-z0-9_]{0,62}$/.test(schema)||!uuid.test(scope.tenant_id)||(scope.kind==='company'&&!uuid.test(scope.organization_id)))throw Error('INVALID_RETENTION_SCOPE');
 // Manifest hash binds scope and the complete bounded identity/hash list.
 if(digest({scope,candidates})!==manifestSha256)throw Error('RETENTION_MANIFEST_CONFLICT');
 const evidence=await evidencePool.connect(),source=await sourcePool.connect();
 try{
  await evidence.query('BEGIN');const e=(await evidence.query('SELECT * FROM kabipay_ops.organization_retention_evidence WHERE id=$1 FOR SHARE',[evidenceId])).rows[0];const decision=retentionDecision(e,{manifestSha256,now});
  if(!decision.allowed)throw Error('RETENTION_EVIDENCE_INCOMPLETE');if(e.source_kind!==scope.kind||e.source_tenant_id!==scope.tenant_id||(scope.kind==='company'&&e.organization_id!==scope.organization_id))throw Error('RETENTION_EVIDENCE_SCOPE');
  const cutover=(await evidence.query('SELECT * FROM kabipay_ops.organization_delivery_cutover WHERE id=$1 AND source_kind=$2 AND source_tenant_id=$3 FOR SHARE',[e.cutover_id,scope.kind,scope.tenant_id])).rows[0];
  const requiredRoles=scope.kind==='company'?['inbox','history','reporting']:['control_history'];
  if(!cutover||cutover.organization_id!==(scope.organization_id??null)||!cutover.legacy_consumer_group||Date.parse(cutover.accepted_at)>now||Date.parse(cutover.legacy_stopped_at)>Date.parse(cutover.accepted_at)||Date.parse(cutover.replacements_verified_at)>Date.parse(cutover.accepted_at)||!hash.test(cutover.reconciliation_sha256)||typeof cutover.legacy_final_offsets!=='object'||Object.keys(cutover.legacy_final_offsets).length===0||!requiredRoles.every(r=>cutover.replacement_roles.includes(r)))throw Error('LEGACY_CUTOVER_NOT_ACCEPTED');
  if(scope.kind==='company'){const healthy=(await evidence.query("SELECT kabipay_ops.organization_capture_healthy($1,$2,ARRAY['inbox','history','reporting']) AS healthy",[scope.organization_id,scope.tenant_id])).rows[0]?.healthy;if(!healthy)throw Error('RETENTION_CAPTURE_INCOMPLETE');}
  let policy;
  if(execute){
   if(executionPolicy?.enableExecute!==true||!uuid.test(executionPolicy.id)||!Number.isSafeInteger(executionPolicy.version)||stable(executionPolicy.binding?.scope)!==stable(scope)||typeof executionPolicy.binding?.database!=='string')throw Error('RETENTION_EXECUTION_DISABLED');
   policy=(await evidence.query('SELECT * FROM kabipay_ops.organization_retention_policy WHERE id=$1 AND version=$2 FOR SHARE',[executionPolicy.id,executionPolicy.version])).rows[0];
   if(!policy||e.retention_policy_id!==policy.id||policy.state!=='ACTIVE'||policy.execute_enabled!==true||policy.require_restore_verified!==true||Date.parse(policy.accepted_at)>now||Date.parse(policy.expires_at)<=now||policy.source_kind!==scope.kind||policy.organization_id!==(scope.organization_id??null)||policy.source_tenant_id!==scope.tenant_id||policy.source_schema!==schema||policy.source_database!==executionPolicy.binding.database||candidates.length>policy.max_batch||!Number.isSafeInteger(Number(policy.minimum_age_seconds))||Date.parse(e.retention_before)>now-Number(policy.minimum_age_seconds)*1000)throw Error('RETENTION_POLICY_NOT_ACCEPTED');
  }
  await source.query('BEGIN');await source.query("SET LOCAL lock_timeout='2s';SET LOCAL statement_timeout='10s'");
  if(scope.kind==='company'){
   await source.query('SELECT pg_advisory_xact_lock(hashtextextended($1,0))',[['organization-history-retention',scope.tenant_id,scope.organization_id].join(':')]);
   if((await source.query(`SELECT id FROM "${schema}".organization_history_export WHERE tenant_id=$1 AND organization_id=$2 AND state='PENDING' LIMIT 1 FOR SHARE`,[scope.tenant_id,scope.organization_id])).rowCount)throw Error('SOURCE_HISTORY_EXPORT_PENDING');
  }
  if(execute){const identity=(await source.query('SELECT current_database() AS database')).rows[0];if(identity.database!==executionPolicy.binding.database)throw Error('RETENTION_SOURCE_BINDING');if(executionPolicy.requireSyntheticFixture===true){if(!/^organization_gap_fixture_[a-f0-9]{32}(_src)?$/.test(identity.database)||typeof fixtureNonce!=='string'||fixtureNonce.length<32)throw Error('SYNTHETIC_ONLY_EXECUTION');const fixture=(await source.query('SELECT nonce FROM public.organization_synthetic_fixture WHERE nonce=$1 FOR SHARE',[fixtureNonce])).rows[0];if(!fixture)throw Error('SYNTHETIC_FIXTURE_PROOF_REQUIRED');}}
  const table=scope.kind==='company'?'company_event_outbox':'organization_outbox';
  const body=scope.kind==='company'?'event_envelope::text':'to_jsonb(o)::text';
  const rows=(await source.query(`SELECT id,source_tenant_id,organization_id,created_at,encode(sha256(convert_to(${body},'UTF8')),'hex') AS sha256${scope.kind==='control'?',to_jsonb(o) AS source_record':''} FROM "${schema}".${table} o WHERE id=ANY($1::uuid[]) ORDER BY id FOR UPDATE`,[candidates.map(c=>c.id)])).rows;
  if(rows.length!==candidates.length||rows.some(r=>r.source_tenant_id!==scope.tenant_id||(scope.kind==='company'&&r.organization_id!==scope.organization_id)||Date.parse(r.created_at)>=Date.parse(e.retention_before)||candidates.find(c=>c.id===r.id)?.sha256!==r.sha256))throw Error('RETENTION_CANDIDATE_CHANGED');
  if(scope.kind==='control'&&(typeof verifyControlCoverage!=='function'||await verifyControlCoverage({rows,cutover,evidence:e})!==true))throw Error('CONTROL_HISTORY_COVERAGE_INCOMPLETE');
  // Hold evidence lease lock and source candidate locks, then recheck wall time
  // immediately before deletion. A stale approved file alone cannot authorize.
  if(!retentionDecision(e,{manifestSha256,now:Date.now()}).allowed)throw Error('RETENTION_EVIDENCE_EXPIRED');
  if(execute&&Date.parse(policy.expires_at)<=Date.now())throw Error('RETENTION_POLICY_EXPIRED');
  if(scope.kind==='company'&&(await evidence.query("SELECT kabipay_ops.organization_capture_healthy($1,$2,ARRAY['inbox','history','reporting']) AS healthy",[scope.organization_id,scope.tenant_id])).rows[0]?.healthy!==true)throw Error('RETENTION_CAPTURE_EXPIRED');
  let deleted=0;if(execute){const result=await source.query(`DELETE FROM "${schema}".${table} WHERE id=ANY($1::uuid[])`,[rows.map(r=>r.id)]);deleted=result.rowCount;if(deleted!==candidates.length)throw Error('RETENTION_DELETE_COUNT');await source.query(`INSERT INTO "${schema}".organization_outbox_cleanup_receipt(id,evidence_id,policy_id,policy_version,source_kind,source_tenant_id,organization_id,manifest_sha256,candidate_ids,deleted_rows) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)`,[randomUUID(),evidenceId,policy.id,executionPolicy.version,scope.kind,scope.tenant_id,scope.organization_id??null,manifestSha256,rows.map(r=>r.id),deleted]);}
  await source.query('COMMIT');await evidence.query('COMMIT');return {mode:execute?'EXECUTION':'DRY_RUN',eligible:rows.length,deleted,manifest_sha256:manifestSha256};
 }catch(error){await source.query('ROLLBACK');await evidence.query('ROLLBACK');throw error;}finally{source.release();evidence.release();}
}
