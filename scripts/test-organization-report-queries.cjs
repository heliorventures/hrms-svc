// Fixed query smoke check against the owned synthetic ClickHouse endpoint; no application data output.
const fs=require('node:fs');
const source=fs.readFileSync('crates/kabipay-organization/src/operational_reporting.rs','utf8');
(async()=>{
 if(process.argv.includes('--apply-views')){
  if(process.env.ORGANIZATION_SYNTHETIC_ACCEPTANCE!=='true')throw Error('explicit synthetic acceptance required');
  const ddl=fs.readFileSync(require('node:path').join(__dirname,'../../hrms-clickhouse/migrations/0001-company-history.sql'),'utf8');
  for(const sql of ddl.split(';').map(s=>s.slice(s.indexOf('CREATE OR REPLACE VIEW'))).filter(s=>s.startsWith('CREATE OR REPLACE VIEW'))){
   const u=new URL('http://127.0.0.1:18123/');u.searchParams.set('query',sql);
   const r=await fetch(u,{method:'POST',headers:{'x-clickhouse-user':'acceptance','x-clickhouse-key':'synthetic-only-password'}});if(!r.ok)throw Error(await r.text());
  }
 }
 for(const name of ['ATTENDANCE','LEAVE','PROJECT']){
 const sql=source.match(new RegExp('const '+name+'[^=]*=\\s*"([^"]+)"'))[1];
 const url=new URL('http://127.0.0.1:18123/');url.searchParams.set('query',sql);
 for(const [k,v]of Object.entries({organization:'11111111-1111-4111-8111-111111111111',tenants:"['11111111-1111-4111-8111-111111111111']",from:'2026-10-19',to:'2026-10-19'}))url.searchParams.set('param_'+k,v);
 const response=await fetch(url,{method:'POST',headers:{'x-clickhouse-user':'acceptance','x-clickhouse-key':'synthetic-only-password'}});
 console.log(name,response.status,response.ok?'OK':await response.text());if(!response.ok)process.exitCode=1;
}})().catch(e=>{console.error(e.message);process.exitCode=1;});
