import {test} from 'node:test';import assert from 'node:assert/strict';import {generateKeyPairSync} from 'node:crypto';import {resolve} from 'node:path';import {validateKeys} from './validate-keys.mjs';
const {publicKey}=generateKeyPairSync('rsa',{modulusLength:2048});const pem=publicKey.export({type:'spki',format:'pem'});const a='c52b8e63-b568-4e7c-9109-8746dfe09983',b='c2790e87-bc19-430c-b276-7c7bfb813793';
const key=(purpose,kid,tenant=a)=>({purpose,kid,tenant_ids:[tenant],public_key_file:resolve('synthetic-public.pem')});
test('public-only single-company keys validate',()=>assert.equal(validateKeys({keys:[key('client','a')]},()=>pem).keys,1));
test('canonical DER rejects cross-company and cross-purpose aliases',()=>{for(const other of [key('client','b',b),key('delegation','b')])assert.throws(()=>validateKeys({keys:[key('client','a'),other]},()=>pem),/KEY_REUSED/);});
test('multi-company client and private verification material rejected',()=>{assert.throws(()=>validateKeys({keys:[{...key('client','a'),tenant_ids:[a,b]}]},()=>pem),/BINDING/);assert.throws(()=>validateKeys({keys:[key('client','a')]},()=>Buffer.from('PRIVATE KEY')),/PUBLIC_KEY/);});
