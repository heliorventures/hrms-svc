import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import pg from 'pg';
if(process.env.ORGANIZATION_SYNTHETIC_ACCEPTANCE!=='true')throw Error('EXPLICIT_SYNTHETIC_OPT_IN_REQUIRED');
const admin=new pg.Client({connectionString:'postgres://acceptance:synthetic-only-password@127.0.0.1:15436/organization_acceptance'});
const database='organization_projection_schema_'+Date.now();
const role='organization_projection_gate_'+Date.now();
const client=new pg.Client({connectionString:`postgres://acceptance:synthetic-only-password@127.0.0.1:15436/${database}`});
let created=false,connected=false,roleCreated=false;
try{
 await admin.connect();await admin.query(`CREATE DATABASE ${database}`);created=true;await client.connect();connected=true;
 await client.query("CREATE SCHEMA kabipay_ops; CREATE TABLE kabipay_ops.organization(id uuid PRIMARY KEY,status text NOT NULL DEFAULT 'ACTIVE'); CREATE TABLE kabipay_ops.tenant(id uuid PRIMARY KEY,status text NOT NULL DEFAULT 'ACTIVE',is_deleted boolean NOT NULL DEFAULT false,private_database_url text); CREATE TABLE kabipay_ops.principal(id uuid PRIMARY KEY); CREATE TABLE kabipay_ops.organization_company(id uuid PRIMARY KEY,organization_id uuid NOT NULL,tenant_id uuid NOT NULL,version bigint NOT NULL DEFAULT 1,installed_version bigint DEFAULT 1,runtime_status text NOT NULL DEFAULT 'ACTIVE',status text NOT NULL DEFAULT 'ACTIVE',effective_from timestamptz NOT NULL DEFAULT now(),effective_to timestamptz)");
 const migration=readFileSync(new URL('../../../../hrms-database/changelog/migrations/0116_organization_projections/organization_projections.xml',import.meta.url),'utf8');await client.query(migration.match(/<!\[CDATA\[([\s\S]*?)\]\]>/)[1]);
 const org='00000000-0000-4000-8000-000000000001',tenant='00000000-0000-4000-8000-000000000002';
 await client.query('INSERT INTO kabipay_ops.organization VALUES($1);',[org]);await client.query('INSERT INTO kabipay_ops.tenant VALUES($1)',[tenant]);
 await client.query('INSERT INTO kabipay_ops.organization_company(id,organization_id,tenant_id) VALUES($1,$2,$3)',[org,org,tenant]);
 await admin.query(`CREATE ROLE ${role}`);roleCreated=true;await client.query(`GRANT USAGE ON SCHEMA kabipay_ops TO ${role}; GRANT EXECUTE ON FUNCTION kabipay_ops.organization_capture_member_active(uuid,uuid) TO ${role}; GRANT EXECUTE ON FUNCTION kabipay_ops.organization_capture_member_classification(uuid,uuid) TO ${role}; SET ROLE ${role}`);
 assert.equal((await client.query('SELECT kabipay_ops.organization_capture_member_active($1,$2) AS active',[org,tenant])).rows[0].active,true);
 await assert.rejects(client.query('SELECT private_database_url FROM kabipay_ops.tenant'),e=>e.code==='42501');
 await assert.rejects(client.query("UPDATE kabipay_ops.organization_company SET status='ACTIVE'"),e=>e.code==='42501');await client.query('RESET ROLE');
 await client.query("UPDATE kabipay_ops.organization_company SET status='SUSPENDED'");assert.equal((await client.query('SELECT kabipay_ops.organization_capture_member_active($1,$2) AS active',[org,tenant])).rows[0].active,false);await client.query("UPDATE kabipay_ops.organization_company SET status='ACTIVE'");
 for(const [change,reset] of [["UPDATE kabipay_ops.organization_company SET effective_to=now()-interval '1 second'","UPDATE kabipay_ops.organization_company SET effective_to=NULL"],["UPDATE kabipay_ops.organization SET status='SUSPENDED'","UPDATE kabipay_ops.organization SET status='ACTIVE'"],["UPDATE kabipay_ops.tenant SET is_deleted=true","UPDATE kabipay_ops.tenant SET is_deleted=false"]]){await client.query(change);assert.equal((await client.query('SELECT kabipay_ops.organization_capture_member_active($1,$2) AS active',[org,tenant])).rows[0].active,false);await client.query(reset);}
 await client.query("INSERT INTO kabipay_ops.organization_company_capture(organization_id,source_tenant_id,topic) VALUES($1,$2,'fixed-topic')",[org,tenant]);
 assert.equal((await client.query('SELECT acceptance_status,bootstrap_complete FROM kabipay_ops.organization_company_capture')).rows[0].acceptance_status,'PENDING');
 await assert.rejects(client.query("UPDATE kabipay_ops.organization_company_capture SET acceptance_status='ACCEPTED'"),e=>e.code==='23514');
 await assert.rejects(client.query("UPDATE kabipay_ops.organization_company_capture SET expected_partitions=ARRAY[-1]"),e=>e.code==='23514');
 await assert.rejects(client.query("UPDATE kabipay_ops.organization_company_capture SET expected_partitions=ARRAY[NULL]::int[]"),e=>e.code==='23514');
 await client.query('INSERT INTO kabipay_ops.principal VALUES($1)',[org]);
 await client.query("UPDATE kabipay_ops.organization_company_capture SET acceptance_status='ACCEPTED',membership_id=$1,membership_version=1,bootstrap_complete=true,expected_partitions=ARRAY[0],history_from=now()-interval '1 day',source_bootstrap_through=now(),accepted_at=now(),reconciliation_sha256=repeat('a',64),accepted_by_principal_id=$1",[org]);
 const acceptedQuery='SELECT count(*)::int AS n FROM kabipay_ops.organization_company_capture c JOIN kabipay_ops.organization_company m ON m.id=c.membership_id AND m.organization_id=c.organization_id AND m.tenant_id=c.source_tenant_id AND m.version=c.membership_version';assert.equal((await client.query(acceptedQuery)).rows[0].n,1);await client.query('UPDATE kabipay_ops.organization_company SET version=2');assert.equal((await client.query(acceptedQuery)).rows[0].n,0);
 const key=[org,tenant,'leave_request','00000000-0000-4000-8000-000000000003'];
 const sql=`INSERT INTO kabipay_ops.organization_inbox_projection(organization_id,source_tenant_id,resource_type,resource_id,status,summary,source_version,deleted,occurred_at,payload_hash) VALUES($1,$2,$3,$4,'PENDING','Synthetic',1,false,now(),'hash') RETURNING id`;
 const id=(await client.query(sql,key)).rows[0].id;await client.query("UPDATE kabipay_ops.organization_inbox_projection SET source_version=2");assert.equal((await client.query('SELECT id FROM kabipay_ops.organization_inbox_projection')).rows[0].id,id);
 console.log(JSON.stringify({result:'PASS',migration0116:true,capture_defaults_pending:true,incomplete_acceptance_rejected:true,invalid_partitions_rejected:true,stable_inbox_cursor:true,restricted_membership_gate:true,tenant_database_configuration_denied:true,suspended_expired_deleted_membership_denied:true,capture_membership_version_fence:true}));
}finally{if(connected)await client.end();if(created)await admin.query(`DROP DATABASE ${database}`);if(roleCreated)await admin.query(`DROP ROLE ${role}`);await admin.end();}
