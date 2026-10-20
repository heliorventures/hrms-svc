import {readFileSync} from 'node:fs';
import {processConsumerRecord} from '../src/consumers.mjs';

// Fixture convenience only: every role still runs its production transaction and
// checkpoint independently. Worker-process outage coverage lives in gap-lifecycle.
export async function consumeFixtureRecord(args) {
 let result;
 for(const role of ['inbox','history','reporting']) {
  const history=role==='reporting'?Object.assign(Object.create(args.history),{table:'reporting_history'}):args.history;
  result=await processConsumerRecord({...args,history,role});
 }
 return result;
}
export async function installConsumerSchema(database) {
 const xml=readFileSync(new URL('../../../../hrms-database/changelog/migrations/0120_organization_capture_lifecycle/organization_capture_lifecycle.xml',import.meta.url),'utf8');
 await database.query(xml.match(/<!\[CDATA\[([\s\S]*?)\]\]>/)[1]);
}

export async function installProjectionFixture(database,{org,tenant,id}) {
 await database.query("CREATE SCHEMA kabipay_ops; CREATE TABLE kabipay_ops.organization(id uuid PRIMARY KEY,status text NOT NULL DEFAULT 'ACTIVE'); CREATE TABLE kabipay_ops.tenant(id uuid PRIMARY KEY,status text NOT NULL DEFAULT 'ACTIVE',is_deleted boolean NOT NULL DEFAULT false); CREATE TABLE kabipay_ops.principal(id uuid PRIMARY KEY); CREATE TABLE kabipay_ops.organization_company(id uuid PRIMARY KEY,organization_id uuid NOT NULL,tenant_id uuid NOT NULL,version bigint NOT NULL DEFAULT 1,installed_version bigint DEFAULT 1,runtime_status text NOT NULL DEFAULT 'ACTIVE',status text NOT NULL DEFAULT 'ACTIVE',effective_from timestamptz NOT NULL DEFAULT now(),effective_to timestamptz)");
 await database.query('INSERT INTO kabipay_ops.organization VALUES($1)',[org]);await database.query('INSERT INTO kabipay_ops.tenant VALUES($1)',[tenant]);await database.query('INSERT INTO kabipay_ops.organization_company(id,organization_id,tenant_id) VALUES($1,$2,$3)',[id,org,tenant]);
 const xml=readFileSync(new URL('../../../../hrms-database/changelog/migrations/0116_organization_projections/organization_projections.xml',import.meta.url),'utf8');await database.query(xml.match(/<!\[CDATA\[([\s\S]*?)\]\]>/)[1]);await installConsumerSchema(database);
}
