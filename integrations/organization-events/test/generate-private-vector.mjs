// Synthetic interop fixture generator. Never uses runtime keys or environment.
import {writeFileSync} from 'node:fs';
import {parseKeyring,encryptFrames} from '../src/private/crypto.mjs';
const keyfile={schema_version:1,keys:[{tenant_id:'11111111-1111-4111-8111-111111111111',purpose:'audit',key_id:'test-v1',key_b64:Buffer.alloc(32,7).toString('base64'),active:true}]};
const source=Buffer.from('{"unknown":"é","before":null}\n');
const head={record_id:'22222222-2222-4222-8222-222222222222',tenant_id:keyfile.keys[0].tenant_id,purpose:'audit',source_generation:17,source_kind:'audit_log',resource_type:'UNKNOWN',resource_id:'33333333-3333-4333-8333-333333333333',occurred_at:'2026-10-19T00:00:00.123456Z'};
writeFileSync(new URL('./private-vector.json',import.meta.url),JSON.stringify({keyfile,source_b64:source.toString('base64'),frames:encryptFrames(source,head,parseKeyring(JSON.stringify(keyfile)))},null,2)+'\n');
