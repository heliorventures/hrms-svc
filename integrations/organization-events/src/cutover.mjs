import {digest} from './events.mjs';
// Operator-only fixed deployment workflow. Broker observations, replacement
// coverage and a reconciled archive restore are collected before acceptance.
export async function acceptLegacyCutover({kafkaAdmin,pool,groupId,topics,scope,id,replacementCoverage,reconciliationSha256}){
 if(typeof groupId!=='string'||!/^[a-zA-Z0-9._-]{1,200}$/.test(groupId)||!Array.isArray(topics)||topics.length<1||topics.length>20||typeof replacementCoverage!=='function'||!/^[a-f0-9]{64}$/.test(reconciliationSha256))throw Error('INVALID_CUTOVER_REQUEST');
 const group=(await kafkaAdmin.describeGroups([groupId])).groups[0];if(!group||!['Empty','Dead'].includes(group.state)||group.members.length)throw Error('LEGACY_CONSUMER_STILL_RUNNING');
 const offsets=await kafkaAdmin.fetchOffsets({groupId,topics});if(offsets.length!==topics.length||offsets.some(t=>!t.partitions.length||t.partitions.some(p=>!/^\d+$/.test(p.offset))))throw Error('LEGACY_OFFSETS_UNCONFIRMED');
 if(await replacementCoverage(offsets)!==true)throw Error('REPLACEMENT_COVERAGE_INCOMPLETE');
 // Check once more after external work, before storing the exact broker fence.
 const again=(await kafkaAdmin.describeGroups([groupId])).groups[0];if(!again||!['Empty','Dead'].includes(again.state)||again.members.length||digest(await kafkaAdmin.fetchOffsets({groupId,topics}))!==digest(offsets))throw Error('LEGACY_CUTOVER_CHANGED');
 await pool.query(`INSERT INTO kabipay_ops.organization_delivery_cutover(id,source_kind,organization_id,source_tenant_id,legacy_consumer_group,legacy_final_offsets,replacement_roles,reconciliation_sha256,legacy_stopped_at,replacements_verified_at,accepted_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,clock_timestamp(),clock_timestamp(),clock_timestamp())`,[id,scope.kind,scope.organization_id??null,scope.tenant_id,groupId,JSON.stringify(offsets),scope.kind==='company'?['inbox','history','reporting']:['control_history'],reconciliationSha256]);return {id,offsets};
}
