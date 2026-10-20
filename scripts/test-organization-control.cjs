// Synthetic PostgreSQL only. No dotenv, existing database or image pull.
const {spawnSync}=require('node:child_process');
const fs=require('node:fs');const path=require('node:path');const {randomUUID}=require('node:crypto');
const assert=require('node:assert/strict');
const {Client}=require('../../hrms-database/node_modules/pg');
const root=path.resolve(__dirname,'..');
const name='hrms-org-control-'+randomUUID().replaceAll('-','');
let created=false,db;
function docker(args){const r=spawnSync('docker',args,{encoding:'utf8',windowsHide:true,timeout:60000});if(r.status!==0)throw Error('disposable Docker operation failed: '+r.stderr);return r.stdout.trim();}
async function main(){try{
  if(!process.argv.includes('--prepared')){const r=spawnSync('cargo',['test','-p','kabipay-organization','--test','control_plane','--offline','--no-run'],{cwd:root,stdio:'inherit',windowsHide:true});assert.equal(r.status,0);}
  docker(['run','--pull=never','--rm','-d','--name',name,'--memory','256m','--cpus','1','-p','127.0.0.1::5432','-e','POSTGRES_USER=organization_fixture','-e','POSTGRES_PASSWORD=synthetic','-e','POSTGRES_DB=organization_fixture','postgres:16-alpine']);created=true;
  const port=docker(['port',name,'5432/tcp']).match(/^127\.0\.0\.1:(\d+)$/)?.[1];assert.ok(port);
  let ready=false;for(let i=0;i<80;i++){const r=spawnSync('docker',['exec',name,'pg_isready','-h','127.0.0.1','-U','organization_fixture'],{stdio:'ignore',windowsHide:true});if(r.status===0){ready=true;break;}await new Promise(r=>setTimeout(r,150));}assert.ok(ready);
  const url=`postgres://organization_fixture:synthetic@127.0.0.1:${port}/organization_fixture?sslmode=disable`;
  db=new Client({connectionString:url});await db.connect();
  const tenants=[randomUUID(),randomUUID(),randomUUID()];
  await db.query("CREATE SCHEMA kabipay_ops; CREATE TABLE kabipay_ops.tenant(id UUID PRIMARY KEY,name TEXT NOT NULL DEFAULT 'Synthetic Company',status TEXT NOT NULL DEFAULT 'ACTIVE',is_deleted BOOLEAN NOT NULL DEFAULT false)");
  for(const id of tenants)await db.query('INSERT INTO kabipay_ops.tenant(id) VALUES($1)',[id]);
  const migrations=['0111_organization_foundation/organization_foundation.xml'];
  if(!process.argv.includes('--baseline'))migrations.push('0113_organization_control_outbox/organization_control_outbox.xml','0114_organization_execution/organization_execution.xml','0116_organization_projections/organization_projections.xml');
  for(const file of migrations){const xml=fs.readFileSync(path.resolve(root,'../hrms-database/changelog/migrations',file),'utf8').replace(/<rollback>[\s\S]*?<\/rollback>/g,'');for(const match of xml.matchAll(/<sql\s+splitStatements="false">\s*<!\[CDATA\[([\s\S]*?)\]\]>\s*<\/sql>/g))await db.query(match[1]);}
  const r=spawnSync('cargo',['test','-p','kabipay-organization','--test','control_plane','--offline','--','--ignored'],{cwd:root,stdio:'inherit',windowsHide:true,env:{...process.env,KABIPAY_ORGANIZATION_FIXTURE_URL:url,KABIPAY_ORGANIZATION_FIXTURE_TENANTS:JSON.stringify(tenants)}});assert.equal(r.status,0,'control-plane scenario failed');
}finally{if(db)await db.end();if(created)docker(['rm','-f',name]);}}
main().catch(error=>{console.error(error.message);process.exitCode=1;});
