import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import {validateCaptureBindings} from '../src/capture-bindings.mjs';
import {connectorConfirmation} from '../src/connect-checkpoint.mjs';
const tenant='00000000-0000-4000-8000-000000000001',other='00000000-0000-4000-8000-000000000002';
function binding(id=tenant) {return {tenant_id:id,topic_prefix:`organization.company.${id}`,source_schema:'public',source_database:'company',company_topic:`organization.company.${id}`,organization_ids:[tenant],expected_partitions:[0],connector_name:`organization-company-${id}`,connect_rest_url:`https://capture-connect-${id}:8443`};}
function entries(b){return {[`${b.topic_prefix}.public.organization_capture_marker`]:b};}

test('capture proxy selection is fixed to the trusted company binding',()=>{
 assert.equal(validateCaptureBindings(entries(binding()))[Object.keys(entries(binding()))[0]].tenant_id,tenant);
 for(const url of [`https://capture-connect-${other}:8443`,`https://capture-connect-${tenant}:443`,`https://connect:8443`,`http://capture-connect-${tenant}:8443`,`https://u:p@capture-connect-${tenant}:8443`,`https://capture-connect-${tenant}:8443/private`,`https://capture-connect-${tenant}:8443/?target=other`,`https://capture-connect-${tenant}:8443/#fragment`,undefined])assert.throws(()=>validateCaptureBindings(entries({...binding(),connect_rest_url:url})),/INVALID_CONNECT_CAPTURE_BINDING/);
 assert.throws(()=>validateCaptureBindings(entries({...binding(),connect_rest_url:'http://127.0.0.1:18083'})),/INVALID_CONNECT_CAPTURE_BINDING/);
 assert.throws(()=>validateCaptureBindings({}),/INVALID_CAPTURE_BINDING/);
});

test('two company confirmations use independent trusted HTTP fixture endpoints',async()=>{
 const seen=[[],[]],companies=[binding(),binding(other)],servers=[];
 try{
  for(let i=0;i<2;i++){
   const server=http.createServer((request,response)=>{seen[i].push(request.url);response.setHeader('content-type','application/json');response.end(JSON.stringify({offsets:[{partition:{server:companies[i].topic_prefix},offset:{lsn:'2',lsn_commit:'2'}}]}));});
   servers.push(server);await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
   companies[i].connect_rest_url=`http://127.0.0.1:${server.address().port}`;
  }
  const trusted=validateCaptureBindings({...entries(companies[0]),...entries(companies[1])},{isolated:true});
  for(const b of Object.values(trusted))assert.equal(await connectorConfirmation({url:b.connect_rest_url,isolated:true})({cdc_lsn:'0/2'},b),true);
  for(let i=0;i<2;i++)assert.deepEqual(seen[i],[`/connectors/${companies[i].connector_name}/offsets`]);
 }finally{for(const server of servers){server.closeAllConnections();await new Promise(resolve=>server.close(resolve));}}
});
