import test from 'node:test';import assert from 'node:assert/strict';
import {parseControlEvent} from '../src/control-events.mjs';import {envelope,binding} from './control-fixture.mjs';
const routed=()=>({route_schema_version:1,source_database:binding.database,source_schema:binding.schema,event:envelope().after});
const parse=e=>parseControlEvent(Buffer.from(JSON.stringify(e)),binding.topic,binding);
test('versioned routed control preserves native control DTO and content hash',()=>{assert.deepEqual(parse(routed()),parse(envelope()));});
test('EventRouter JSON string preserves explicit null organization without widening native input',()=>{
 assert.deepEqual(parse(JSON.stringify(routed())),parse(envelope()));
 assert.throws(()=>parse(JSON.stringify(envelope())));
});
test('versioned routed control rejects altered source scope, private bodies, unknown keys and versions',()=>{
 for(const change of [e=>e.source_database='other',e=>e.source_schema='other',e=>e.route_schema_version=2,e=>e.unexpected=true,e=>e.event.source_position=1,e=>e.event.payload={schema_version:1,purpose:'operator_audit',ciphertext:'private'}]){const e=routed();change(e);assert.throws(()=>parse(e));}
 assert.throws(()=>parseControlEvent(Buffer.from(JSON.stringify(routed())),'organization.operator_audit.metadata',binding));
});
