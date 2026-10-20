// Run at the fixed source deployment, never in the organization browser API.
import {readFileSync} from 'node:fs';import pg from 'pg';import {cleanupOutbox} from './retention.mjs';
import {ControlHttp} from './control-http.mjs';import {controlRetentionVerifier} from './control-retention.mjs';
const required=k=>{if(!process.env[k])throw Error(`MISSING_${k}`);return process.env[k];};
const request=JSON.parse(readFileSync(required('RETENTION_REQUEST_FILE'),'utf8'));
const executionPolicy=process.env.RETENTION_OPERATOR_CONFIG_FILE?JSON.parse(readFileSync(process.env.RETENTION_OPERATOR_CONFIG_FILE,'utf8')):{enableExecute:false};
const verifyControlCoverage=request.scope?.kind==='control'?controlRetentionVerifier({binding:JSON.parse(readFileSync(required('CONTROL_BINDING_FILE'),'utf8')),history:new ControlHttp({url:required('CONTROL_RETENTION_CLICKHOUSE_URL'),user:required('CONTROL_RETENTION_CLICKHOUSE_USER'),password:required('CONTROL_RETENTION_CLICKHOUSE_PASSWORD')})}):undefined;
function connection(key,ca){const connectionString=required(key),url=new URL(connectionString);const isolated=process.env.ALLOW_ISOLATED_PLAINTEXT==='true';if(isolated&&!['127.0.0.1','[::1]'].includes(url.hostname))throw Error('FIXTURE_LOOPBACK_REQUIRED');return new pg.Pool({connectionString,max:1,ssl:isolated?false:{ca:readFileSync(required(ca),'utf8'),rejectUnauthorized:true}});}
const sourcePool=connection('SOURCE_OUTBOX_DATABASE_URL','SOURCE_POSTGRES_CA_FILE'),evidencePool=connection('ORGANIZATION_METADATA_DATABASE_URL','POSTGRES_CA_FILE');
try{const result=await cleanupOutbox({...request,sourcePool,evidencePool,executionPolicy,verifyControlCoverage,execute:process.env.RETENTION_EXECUTE==='true',fixtureNonce:process.env.ORGANIZATION_SYNTHETIC_FIXTURE_NONCE});console.log(JSON.stringify(result));}catch{console.error(JSON.stringify({code:'RETENTION_REJECTED'}));process.exitCode=1;}finally{await sourcePool.end();await evidencePool.end();}
