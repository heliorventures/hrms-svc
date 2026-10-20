import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {ClickHouseHistory} from '../src/history.mjs';
import {parseEvent} from '../src/events.mjs';
if(process.env.ORGANIZATION_SYNTHETIC_ACCEPTANCE!=='true')throw Error('EXPLICIT_SYNTHETIC_OPT_IN_REQUIRED');
process.env.ALLOW_ISOLATED_PLAINTEXT='true';
const module=readFileSync(new URL('../../../crates/kabipay-organization/src/reporting.rs',import.meta.url),'utf8');
const historySql=module.match(/const HISTORY_SQL: &str\s*=\s*"([^"]+)";/)?.[1];
const progressSql=module.match(/const PROGRESS_SQL: &str\s*=\s*"([^"]+)";/)?.[1];
assert(historySql&&progressSql,'reporting query constants missing');
const h=new ClickHouseHistory({url:'http://127.0.0.1:18123',user:'acceptance',password:'synthetic-only-password'});
await h.query("ALTER TABLE organization.business_history ADD COLUMN IF NOT EXISTS actor String DEFAULT 'null'");
const scope={organization:'00000000-0000-4000-8000-000000000001',tenants:"['00000000-0000-4000-8000-000000000002']"};
async function checkedQuery(sql,params){try{return await h.query(sql,params);}catch(error){const u=new URL('http://127.0.0.1:18123');u.searchParams.set('query',sql);for(const [k,v] of Object.entries(params))u.searchParams.set('param_'+k,String(v));const r=await fetch(u,{method:'POST',headers:{'X-ClickHouse-User':'acceptance','X-ClickHouse-Key':'synthetic-only-password'}});console.error('SYNTHETIC_QUERY_DIAGNOSTIC',await r.text());throw error;}}
const historyRows=(await checkedQuery(historySql,{...scope,from:'2026-10-19 00:00:00.000',to:'2026-10-19 23:59:59.999',after:'00000000-0000-0000-0000-000000000000',limit:5})).trim().split('\n').filter(Boolean).map(JSON.parse);
assert(historyRows.length>0);assert(historyRows.every(row=>Number.isFinite(Date.parse(row.occurred_at_utc))&&typeof row.payload==='string'&&typeof row.aggregate_version==='string'));
const progressRows=(await checkedQuery(progressSql,{...scope,from:'2026-10-19',to:'2026-10-19'})).trim().split('\n').filter(Boolean).map(JSON.parse);
assert.equal(progressRows[0].logged_seconds,'1800');assert.equal(progressRows[0].approved_seconds,'1800');
console.log(JSON.stringify({result:'PASS',root_rust_history_query:true,typed_uuid_array_and_datetime64:true,history_cursor_rows:historyRows.length,root_rust_progress_query:true,corrected_seconds:progressRows[0].logged_seconds}));
// Exact totals above the JavaScript safe integer boundary, with typed attribution.
const tenant='00000000-0000-4000-8000-000000000002';
for(const suffix of ['991','992']){
 const id='00000000-0000-4000-8000-000000000'+suffix;
 const event={schema_version:1,event_id:id,event_type:'timesheet.changed',organization_id:scope.organization,source_tenant_id:tenant,aggregate_type:'timesheet_entry',aggregate_id:id,aggregate_version:1,occurred_at:'2026-10-19T02:00:00Z',deleted:false,
  actor:{kind:'organization_principal',principal_id:scope.organization,display_name:'Synthetic manager',identity_version:1,source_tenant_id:scope.organization,source_user_id:scope.organization,authority_kind:'organization_grant',authority_id:scope.organization,authority_version:1,command_id:id},
  payload:{employee_id:id,work_date:'2026-10-17',logged_seconds:Number.MAX_SAFE_INTEGER,approved_seconds:0}};
 await h.write(parseEvent(Buffer.from(JSON.stringify(event)),'synthetic',{synthetic:{organization_id:scope.organization,tenant_id:tenant}}));
}
const large=(await checkedQuery(progressSql,{...scope,from:'2026-10-17',to:'2026-10-17'})).trim().split('\n').filter(Boolean).map(JSON.parse);assert.equal(large[0].logged_seconds,(2n*BigInt(Number.MAX_SAFE_INTEGER)).toString());
const actors=JSON.parse(await h.query("SELECT count() AS n FROM organization.business_history FINAL WHERE JSONExtractString(actor,'kind')='organization_principal' FORMAT JSONEachRow"));assert.equal(Number(actors.n),2);
console.log(JSON.stringify({result:'PASS',exact_large_seconds:large[0].logged_seconds,typed_history_actor_rows:Number(actors.n)}));
