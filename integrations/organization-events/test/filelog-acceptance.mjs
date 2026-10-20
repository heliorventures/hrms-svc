import assert from 'node:assert/strict';
import {readFileSync,writeFileSync,mkdtempSync,mkdirSync,rmSync} from 'node:fs';
import {resolve,join} from 'node:path';
import {randomUUID} from 'node:crypto';
import {execFileSync} from 'node:child_process';
if(process.env.ORGANIZATION_SYNTHETIC_ACCEPTANCE!=='true')throw Error('SYNTHETIC_OPT_IN_REQUIRED');
const base=resolve('../../infrastructure/organization/shared/storage');mkdirSync(base,{recursive:true});
const dir=mkdtempSync(join(base,'filelog-acceptance-')),name=`organization-filelog-acceptance-${randomUUID().slice(0,8)}`;
const image='ghcr.io/open-telemetry/opentelemetry-collector-releases/opentelemetry-collector-contrib:0.162.0@sha256:39923a8e431bd1f57be82411999d389fcfe40857492e4365456d97a4c1f74be6';
let started=false;
try{
 let config=readFileSync('../../infrastructure/organization/otel-filelog.template.yaml','utf8');
 config=config.replace('start_at: end','start_at: beginning').replace('/var/log/kabipay/approved-service.json.log','/test/source.json.log').replace(/exporters:\n[\s\S]*?\nservice:/,'exporters:\n  file:\n    path: /test/output.json\nservice:').replace('exporters: [otlp]','exporters: [file]');
 writeFileSync(join(dir,'config.yaml'),config);
 const privateMarker='SYNTHETIC_PRIVATE_TOKEN_AND_EMPLOYEE_BODY';
 const structured={level:'WARN',target:'kabipay_synthetic',fields:{code:'SYNTHETIC_FAILURE',message:privateMarker,authorization:privateMarker,tenant_id:'forged-company'},span:{employee_name:privateMarker},tenant_id:'forged-company'};
 writeFileSync(join(dir,'source.json.log'),JSON.stringify({log:JSON.stringify(structured)+'\n',stream:'stdout',time:new Date().toISOString()})+'\n');
 execFileSync('docker',['run','-d','--name',name,'--network','none','--tmpfs','/var/lib/otelcol/filelog','-v',`${dir.replaceAll('\\','/')}:/test`,'-e','SOURCE_SERVICE_NAME=kabipay-synthetic-source','-e','SOURCE_TENANT_ID=00000000-0000-4000-8000-000000000002',image,'--config=/test/config.yaml'],{stdio:'pipe',timeout:30000});started=true;
 let output='';for(let attempt=0;attempt<30;attempt++){await new Promise(r=>setTimeout(r,500));try{output=readFileSync(join(dir,'output.json'),'utf8');if(output.includes('RUST_STRUCTURED_LOG'))break;}catch{}}
 assert(output.includes('RUST_STRUCTURED_LOG'),'actual filelog output required');assert(output.includes('SYNTHETIC_FAILURE'));assert(output.includes('kabipay-synthetic-source'));assert(output.includes('00000000-0000-4000-8000-000000000002'));assert(!output.includes(privateMarker));assert(!output.includes('forged-company'));assert(!output.includes('authorization'));assert(!output.includes('employee_name'));
 console.log(JSON.stringify({result:'PASS',pinned_collector_actual_filelog:true,docker_wrapper_then_rust_json:true,private_fields_removed:true,forged_tenant_removed:true,static_deployment_binding_preserved:true,no_network_or_host_logs:true}));
}finally{
 if(started){execFileSync('docker',['stop','-t','3',name],{stdio:'pipe',timeout:10000});execFileSync('docker',['rm',name],{stdio:'pipe',timeout:10000});}
 assert(dir.startsWith(base+ '\\')||dir.startsWith(base+'/'),'cleanup stays under isolated storage');rmSync(dir,{recursive:true,force:true});
}
