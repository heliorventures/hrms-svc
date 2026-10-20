// Install only with the bound source database. Never expose as a shared worker RPC.
import {readFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
import pg from 'pg';
import {parseKeyring} from './crypto.mjs';
import {PrivateClickHouse} from './clickhouse.mjs';
import {LegacyExporter,ExportFiles,validateBinding,LEGACY_TABLES} from './legacy-export.mjs';
import {restoreExport} from './legacy-reconcile.mjs';
export function operatorConfig(env=process.env){const need=k=>{if(!env[k])throw Error('PRIVATE_OPERATOR_CONFIG');return env[k];},binding=validateBinding({tenant_id:need('PRIVATE_SOURCE_SCOPE_ID'),manifest_id:need('PRIVATE_EXPORT_ID'),source_database:need('PRIVATE_SOURCE_DATABASE'),source_schema:need('PRIVATE_SOURCE_SCHEMA'),source_table:need('PRIVATE_SOURCE_TABLE'),source_generation:Number(need('PRIVATE_SOURCE_GENERATION'))}),url=new URL(need('PRIVATE_SOURCE_DATABASE_URL')),isolated=env.PRIVATE_ISOLATED_FIXTURE==='true';if(!['postgres:','postgresql:'].includes(url.protocol)||[...url.searchParams.keys()].some(k=>/^(ssl|uselibpqcompat)/i.test(k))||isolated&&!['127.0.0.1','localhost','[::1]'].includes(url.hostname))throw Error('PRIVATE_OPERATOR_DATABASE');return {binding,connectionString:url.toString(),isolated,ca:isolated?null:readFileSync(need('PRIVATE_SOURCE_CA_FILE'),'utf8'),keys:parseKeyring(readFileSync(need('KABIPAY_PRIVATE_EVENT_KEYS_FILE'),'utf8')),exportDirectory:need('PRIVATE_EXPORT_DIRECTORY'),need};}
export class SourceLegacyApi {
 constructor(pool,binding){this.pool=pool;this.binding=validateBinding(binding);this.schema='"'+binding.source_schema+'"';}
 async bound(){if((await this.pool.query('SELECT current_database() AS name')).rows[0]?.name!==this.binding.source_database)throw Error('PRIVATE_OPERATOR_DATABASE_BINDING');}
 async fence(){await this.bound();return this.pool.query(`SELECT ${this.schema}.private_fence_legacy($1,$2)`,[this.binding.tenant_id,this.binding.source_generation]);}
 async start(){const b=this.binding;await this.bound();await this.pool.query(`SELECT ${this.schema}.private_legacy_export_start($1,$2,$3,$4)`,[b.tenant_id,b.source_table,b.manifest_id,b.source_generation]);const r=(await this.pool.query(`SELECT * FROM ${this.schema}.private_legacy_export WHERE id=$1 AND tenant_id=$2`,[b.manifest_id,b.tenant_id])).rows[0];if(!r)throw Error('PRIVATE_EXPORT_MANIFEST_MISSING');return {...r,manifest_id:r.id,source_generation:Number(r.producer_generation)};}
 async batch(after,size,bytes){return (await this.pool.query(`SELECT ${this.schema}.private_legacy_export_batch($1,$2,$3,$4,$5) AS batch`,[this.binding.tenant_id,this.binding.manifest_id,after,size,bytes])).rows[0].batch;}
 async publish(frames){return this.pool.query(`SELECT ${this.schema}.private_legacy_export_publish($1,$2,$3::jsonb)`,[this.binding.tenant_id,this.binding.manifest_id,JSON.stringify(frames)]);}
 async publishedFrames(id){return (await this.pool.query(`SELECT ${this.schema}.private_legacy_export_published_frames($1,$2,$3) AS frames`,[this.binding.tenant_id,this.binding.manifest_id,id])).rows[0].frames;}
 async accept(proof){await this.bound();await this.pool.query(`SELECT ${this.schema}.private_legacy_export_accept($1,$2,$3::jsonb)`,[this.binding.tenant_id,this.binding.manifest_id,JSON.stringify(proof)]);}
}
export function protectedStore(config){const n=config.need;return new PrivateClickHouse({url:n('PRIVATE_HISTORY_URL'),user:n('PRIVATE_HISTORY_USER'),password:n('PRIVATE_HISTORY_PASSWORD'),purpose:LEGACY_TABLES[config.binding.source_table],isolated:config.isolated});}
export async function main(argv=process.argv.slice(2)){
 if(argv.length!==1||!['fence','export','restore','accept'].includes(argv[0]))throw Error('PRIVATE_OPERATOR_MODE');const c=operatorConfig(),pool=new pg.Pool({connectionString:c.connectionString,max:1,ssl:c.isolated?false:{ca:c.ca,rejectUnauthorized:true}}),source=new SourceLegacyApi(pool,c.binding),files=new ExportFiles(c.exportDirectory);
 try{let result;if(argv[0]==='fence'){await source.fence();result={fenced:true};}else if(argv[0]==='export'){result=await new LegacyExporter({source,files,keys:c.keys,binding:c.binding}).run({maxBatches:Number(process.env.PRIVATE_EXPORT_MAX_BATCHES??20)});}else{const store=protectedStore(c);result=await restoreExport({files,store,keys:c.keys,binding:c.binding,directory:c.need('PRIVATE_RESTORE_DIRECTORY'),acceptedBy:c.need('PRIVATE_ACCEPTED_BY')});if(argv[0]==='accept'){await source.accept(result);await store.insert('coverage',{scope_id:c.binding.tenant_id,manifest_id:c.binding.manifest_id,coverage_kind:'INITIAL_ARCHIVE',source_generation:c.binding.source_generation,source_database:c.binding.source_database,source_schema:c.binding.source_schema,source_table:c.binding.source_table,record_count:result.row_count,source_sha256:result.source_sha256,restored_sha256:result.restore_sha256,acceptance_sha256:result.acceptance_sha256,accepted_at:new Date().toISOString().replace('T',' ').replace('Z',''),expires_at:null,revoked:0});result={accepted:true,manifest_id:c.binding.manifest_id,row_count:result.row_count,source_sha256:result.source_sha256};}}
  console.log(JSON.stringify(result));
 }finally{await pool.end();}
}
if(process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href)main().catch(()=>{console.error(JSON.stringify({code:'PRIVATE_LEGACY_OPERATION_REJECTED'}));process.exitCode=1;});


