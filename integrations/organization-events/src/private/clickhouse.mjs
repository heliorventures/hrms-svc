import {clickhouseQuery} from '../clickhouse-http.mjs';
import {purpose as validPurpose,uuid} from './contract.mjs';
const parseRows=text=>text.trim().split('\n').filter(Boolean).map(line=>JSON.parse(line));
/** Purpose is fixed at process startup and can never be selected by an RPC. */
export class PrivateClickHouse {
 constructor({url,user,password,purpose,timeout=15000,isolated=false}){
  if(!validPurpose(purpose)||typeof user!=='string'||!user||typeof password!=='string'||!password)throw Error('PRIVATE_HISTORY_CONFIG');
  this.url=new URL(url);
  if(this.url.username||this.url.password||this.url.search||this.url.hash||this.url.pathname!=='/'||!(this.url.protocol==='https:'||(isolated&&this.url.protocol==='http:'&&['127.0.0.1','localhost','clickhouse'].includes(this.url.hostname))))throw Error('PRIVATE_HISTORY_TLS');
  this.purpose=purpose;this.headers={'X-ClickHouse-User':user,'X-ClickHouse-Key':password};this.timeout=timeout;
  this.url.searchParams.set('wait_end_of_query','1');
  // Kafka offsets and source positions exceed JavaScript's safe integer range.
  // Pin the wire format even when a server profile changes its default.
  this.url.searchParams.set('output_format_json_quote_64bit_integers','1');
 }
 query(sql,params={},body,maxBytes=64*1024*1024){return clickhouseQuery({url:this.url,headers:this.headers,timeout:this.timeout,maxBytes,errors:{http:()=> 'PRIVATE_HISTORY_STORAGE',size:'PRIVATE_HISTORY_RESPONSE_LIMIT',response:'PRIVATE_HISTORY_STORAGE',unavailable:'PRIVATE_HISTORY_UNAVAILABLE'}},sql,params,body);}
 table(suffix){if(!['record','chunk','checkpoint','coverage'].includes(suffix))throw Error('PRIVATE_HISTORY_TABLE');return `private_history.${this.purpose}_${suffix}`;}
 async insert(suffix,row){await this.query(`INSERT INTO ${this.table(suffix)} SETTINGS async_insert=0 FORMAT JSONEachRow`,{},JSON.stringify(row)+'\n');}
 async chunk(c){return (await this.rows(`SELECT * FROM ${this.table('chunk')} FINAL WHERE scope_id={scope:UUID} AND record_id={id:UUID} AND source_kind={kind:String} AND resource_type={type:String} AND source_generation={generation:UInt64} AND frame_index={frame:UInt16} LIMIT 2`,{scope:c.scope_id,id:c.record_id,kind:c.source_kind,type:c.resource_type,generation:c.source_generation,frame:c.frame_index}))[0];}
 writeChunk(c){return this.insert('chunk',c);}
 frames(c){return this.rows(`SELECT * FROM ${this.table('chunk')} FINAL WHERE scope_id={scope:UUID} AND record_id={id:UUID} AND source_kind={kind:String} AND resource_type={type:String} AND source_generation={generation:UInt64} ORDER BY frame_index LIMIT 65`,{scope:c.scope_id,id:c.record_id,kind:c.source_kind,type:c.resource_type,generation:c.source_generation});}
 async record(r){return (await this.rows(`SELECT * FROM ${this.table('record')} FINAL WHERE scope_id={scope:UUID} AND record_id={id:UUID} AND source_database={database:String} AND source_schema={schema:String} AND source_table={table:String} LIMIT 2`,{scope:r.scope_id,id:r.record_id,database:r.source_database,schema:r.source_schema,table:r.source_table}))[0];}
 writeRecord(r){return this.insert('record',r);}
 async checkpoint(p){
  const previous=await this.rows(`SELECT kafka_offset,record_id,source_sha256 FROM ${this.table('checkpoint')} FINAL WHERE scope_id={scope:UUID} AND source_generation={generation:UInt64} AND topic={topic:String} AND partition_id={partition:UInt32} LIMIT 2`,{scope:p.scope_id,generation:p.source_generation,topic:p.topic,partition:p.partition_id});
  if(previous.length){const last=previous[0];if(BigInt(last.kafka_offset)>BigInt(p.kafka_offset))return;if(BigInt(last.kafka_offset)===BigInt(p.kafka_offset)){if(last.record_id!==p.record_id||last.source_sha256!==p.source_sha256)throw Error('PRIVATE_HISTORY_OFFSET_CONFLICT');return;}}
  return this.insert('checkpoint',p);
 }
 rows(sql,params={},maxBytes){return this.query(sql+' FORMAT JSONEachRow',params,undefined,maxBytes).then(parseRows);}
 async requireCoverage(r){
  if(this.purpose!=='audit'||!uuid(r.tenant_id)||!uuid(r.coverage_id)||!['source_database','source_schema'].every(k=>typeof r[k]==='string'&&r[k].length>0&&r[k].length<=128))throw Error('PRIVATE_HISTORY_COVERAGE_CONFIG');
  const rows=await this.rows(`SELECT manifest_id FROM ${this.table('coverage')} FINAL WHERE scope_id={scope:UUID} AND manifest_id={manifest:UUID} AND source_generation<={generation:UInt64} AND source_database={database:String} AND source_schema={schema:String} AND source_table='audit_log' AND source_sha256={hash:String} AND restored_sha256={hash:String} AND revoked=0 AND accepted_at<=now64(6) AND coverage_kind='INITIAL_ARCHIVE' AND expires_at IS NULL LIMIT 2`,{scope:r.tenant_id,manifest:r.coverage_id,generation:r.source_generation,database:r.source_database,schema:r.source_schema,hash:r.coverage_sha256},2048);
  if(rows.length!==1)throw Error('PRIVATE_HISTORY_COVERAGE_REQUIRED');
 }
 async readAudit(filter){
  if(this.purpose!=='audit'||!uuid(filter.tenant_id)||!Number.isSafeInteger(filter.limit)||filter.limit<1||filter.limit>500||!Number.isSafeInteger(filter.offset)||filter.offset<0||filter.offset>100000||!Array.isArray(filter.actions)||filter.actions.length>64||!['source_database','source_schema'].every(k=>typeof filter[k]==='string'&&/^[A-Za-z0-9_][A-Za-z0-9_-]{0,127}$/.test(filter[k])))throw Error('PRIVATE_HISTORY_FILTER');
  const clauses=['scope_id={tenant:UUID}','tenant_id={tenant:UUID}','source_database={database:String}','source_schema={schema:String}',"source_kind IN ('audit_log','legacy_audit_log')"],params={tenant:filter.tenant_id,database:filter.source_database,schema:filter.source_schema,limit:filter.limit,offset:filter.offset};
  if(filter.entity_type!==null&&filter.entity_type!==undefined){if(typeof filter.entity_type!=='string'||filter.entity_type.length>128)throw Error('PRIVATE_HISTORY_FILTER');clauses.push('entity_type={entity_type:String}');params.entity_type=filter.entity_type;}
  if(filter.entity_id!==null&&filter.entity_id!==undefined){if(!uuid(filter.entity_id))throw Error('PRIVATE_HISTORY_FILTER');clauses.push("JSONExtractString(raw_source,'entity_id')={entity_id:String}");params.entity_id=filter.entity_id;}
  if(filter.actions.length){if(!filter.actions.every(a=>typeof a==='string'&&a.length<=128))throw Error('PRIVATE_HISTORY_FILTER');clauses.push("has(JSONExtract({actions:String},'Array(String)'),action)");params.actions=JSON.stringify(filter.actions);}
  for(const [key,operator] of [['since','>='],['until','<=']])if(filter[key]!==null&&filter[key]!==undefined){if(typeof filter[key]!=='string'||!/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,9})?Z$/.test(filter[key])||!Number.isFinite(Date.parse(filter[key])))throw Error('PRIVATE_HISTORY_FILTER');clauses.push(`occurred_at${operator}parseDateTime64BestEffort({${key}:String},6,'UTC')`);params[key]=filter[key];}
  const where=clauses.join(' AND '),rows=await this.rows(`SELECT raw_source FROM ${this.table('record')} FINAL WHERE ${where} ORDER BY occurred_at DESC,record_id DESC LIMIT {limit:UInt64} OFFSET {offset:UInt64}`,params,8*1024*1024);
  const counts=await this.rows(`SELECT count() AS total FROM ${this.table('record')} FINAL WHERE ${where}`,params,1024);
  const total=Number(counts[0]?.total);if(!Number.isSafeInteger(total)||total<rows.length)throw Error('PRIVATE_HISTORY_COUNT');
  // Raw JSON remains a string across the RPC. JavaScript must not round unknown
  // audit quantities before Rust reconstructs the original JSON values.
  return {records:rows.map(r=>r.raw_source),total};
 }
}
