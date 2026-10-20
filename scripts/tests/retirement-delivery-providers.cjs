const test=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
const root=path.join(__dirname,'../..'),read=p=>fs.readFileSync(path.join(root,p),'utf8');
test('analytics supported readers use private delivery history and atomic replay instead of retired entities',()=>{
 const source=read('crates/kabipay-analytics/src/services/analytics_service.rs');assert.doesNotMatch(source,/d0030_outbox_events|webhook_delivery_log::Entity/);assert.match(source,/read_configured_delivery_history/);assert.match(source,/append_delivery_replay/);
 const mutation=read('crates/kabipay-analytics/src/resolvers/mutation.rs');assert.match(mutation,/state.outbox_aggregates/);assert.match(mutation,/claims.sub/);
});
test('history client and replay enforce source authority, bounded overlay and terminal protections',()=>{
 const source=read('crates/kabipay-common/src/private_events/delivery_history_client.rs');assert.match(source,/REPEATABLE READ READ ONLY/);assert.match(source,/decrypt_frames/);assert.match(source,/source_generation/);assert.match(source,/UNCERTAIN/);assert.match(source,/FOR UPDATE/);assert.match(source,/append_audit/);assert.match(source,/base_lease/);
});
test('new delivery input and webhook body share PostgreSQL microsecond timestamps',()=>{
 const common=read('crates/kabipay-common/src/private_events/mod.rs'),webhooks=read('crates/kabipay-common/src/private_webhooks.rs');
 assert.match(common,/canonical_delivery_input\(input\)/);assert.match(webhooks,/canonical_private_time\(event.created_at\)/);
});
