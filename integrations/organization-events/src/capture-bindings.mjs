import {validatePartitions} from './capture.mjs';
const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

// Only the operator-owned binding file selects a company Connect proxy. Neither
// CDC bodies nor request parameters can supply or override transport addresses.
export function validateCaptureBindings(bindings,{isolated=false}={}) {
 if(!bindings||typeof bindings!=='object'||Array.isArray(bindings)||!Object.keys(bindings).length)throw Error('INVALID_CAPTURE_BINDING');
 for(const [topic,b] of Object.entries(bindings)) {
  if(!b||!uuid.test(b.tenant_id??'')||topic!==`${b.topic_prefix}.${b.source_schema}.organization_capture_marker`||!b.source_database||!b.company_topic||!Array.isArray(b.organization_ids)||!b.organization_ids.length)throw Error('INVALID_CAPTURE_BINDING');
  validatePartitions(b.expected_partitions);
  let endpoint;try{endpoint=new URL(b.connect_rest_url);}catch{throw Error('INVALID_CONNECT_CAPTURE_BINDING');}
  if(endpoint.username||endpoint.password||endpoint.search||endpoint.hash||endpoint.pathname!=='/')throw Error('INVALID_CONNECT_CAPTURE_BINDING');
  const fixedProxy=endpoint.protocol==='https:'&&endpoint.hostname===`capture-connect-${b.tenant_id}`&&endpoint.port==='8443';
  const fixture=isolated&&endpoint.protocol==='http:'&&['127.0.0.1','[::1]'].includes(endpoint.hostname);
  if(!fixedProxy&&!fixture)throw Error('INVALID_CONNECT_CAPTURE_BINDING');
 }
 return bindings;
}
