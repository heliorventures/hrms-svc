import https from 'node:https';
import http from 'node:http';
import {readFileSync} from 'node:fs';
const codes=new Set(['PROJECTION_APPLIED','HISTORY_RETAINED','PROJECTION_QUARANTINED','PROJECTION_RETRY','CONSUMER_CRASH']);
export function otlpRecord(code,context={}) {
 if(!codes.has(code))throw Error('DIAGNOSTIC_CODE_NOT_ALLOWED');
 const attributes=[];
 // No arbitrary context values, payloads, employee names or exception strings.
 for(const key of ['topic','partition','offset'])if(context[key]!==undefined)attributes.push({key:`messaging.${key}`,value:{stringValue:String(context[key]).slice(0,200)}});
 return {resourceLogs:[{resource:{attributes:[{key:'service.name',value:{stringValue:'kabipay-organization-events'}}]},scopeLogs:[{scope:{name:'organization-projections',version:'1'},logRecords:[{timeUnixNano:(BigInt(Date.now())*1000000n).toString(),severityNumber:code.includes('RETRY')||code.includes('CRASH')?17:9,body:{stringValue:code},attributes}]}]}]};
}
export function diagnosticsFromEnvironment() {
 const endpoint=process.env.OTEL_EXPORTER_OTLP_LOGS_ENDPOINT;
 if(!endpoint)return {emit(){}};
 const url=new URL(endpoint),isolated=process.env.ALLOW_ISOLATED_PLAINTEXT==='true';
 if(url.protocol!=='https:'&&!isolated)throw Error('OTEL_TLS_REQUIRED');
 const tls=isolated?{}:{ca:readFileSync(process.env.OTEL_CA_FILE),cert:readFileSync(process.env.OTEL_CERT_FILE),key:readFileSync(process.env.OTEL_KEY_FILE),rejectUnauthorized:true};
 let pending=false;
 return {emit(code,context){
  if(pending)return; // bounded best-effort diagnostics, never business history
  pending=true;
  const body=JSON.stringify(otlpRecord(code,context));
  const request=(url.protocol==='https:'?https:http).request(url,{...tls,method:'POST',headers:{'Content-Type':'application/json','Content-Length':Buffer.byteLength(body)},timeout:1500},response=>{response.resume();response.once('end',()=>{pending=false;});response.once('error',()=>{pending=false;});});
  request.once('error',()=>{pending=false;});request.once('timeout',()=>request.destroy());request.end(body);
 }};
}
