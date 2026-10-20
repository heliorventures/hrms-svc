import test from 'node:test';
import assert from 'node:assert/strict';
import {otlpRecord} from '../src/diagnostics.mjs';
test('diagnostics whitelist transport positions and fixed codes only',()=>{
 const value=JSON.stringify(otlpRecord('PROJECTION_RETRY',{topic:'fixed-topic',partition:0,offset:'123',payload:'private-body',error:'secret',employee_name:'Private Name'}));
 assert(value.includes('fixed-topic'));assert(!value.includes('private-body'));assert(!value.includes('secret'));assert(!value.includes('Private Name'));assert.throws(()=>otlpRecord('private arbitrary error'));
});
