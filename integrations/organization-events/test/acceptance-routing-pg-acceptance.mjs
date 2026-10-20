// PostgreSQL-only fixture conformance. No connector, Kafka or ClickHouse claim.
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import {readFileSync} from 'node:fs';
import pg from 'pg';
import {fixtureConnectors,fixtureCompanyTopic,fixtureControlTopic,fixtureCompanyRoutingDdl,fixtureControlRoutingDdl} from './kafka-fixture.mjs';
import {parseControlEvent} from '../src/control-events.mjs';
if(process.env.ORGANIZATION_SYNTHETIC_ACCEPTANCE!=='true')throw Error('EXPLICIT_SYNTHETIC_OPT_IN_REQUIRED');
const url=new URL(process.env.PRIVATE_LEGACY_FIXTURE_DSN);
if(!['127.0.0.1','localhost'].includes(url.hostname)||url.pathname!=='/private_retirement_fixture'||url.search||url.hash)throw Error('ISOLATED_SOURCE_FIXTURE_REQUIRED');
const database='routing_fixture_'+randomUUID().replaceAll('-',''),target=new URL(url);target.pathname='/'+database;
const admin=new pg.Client({connectionString:url.href,ssl:false,connectionTimeoutMillis:5000}),db=new pg.Client({connectionString:target.href,ssl:false,connectionTimeoutMillis:5000});
const tenant=randomUUID(),org=randomUUID(),principal=randomUUID(),user=randomUUID(),eventId=randomUUID();let created=false,connected=false;
const migration=file=>readFileSync(new URL('../../../../hrms-database/changelog/migrations/'+file,import.meta.url),'utf8').match(/<!\[CDATA\[([\s\S]*?)\]\]>/)[1];
try{
 await admin.connect();assert.equal((await admin.query('SELECT current_database() AS name')).rows[0].name,'private_retirement_fixture');
 await admin.query(`CREATE DATABASE ${database}`);created=true;await db.connect();connected=true;
 await db.query('CREATE SCHEMA company_fixture;CREATE TABLE company_fixture.company_event_outbox(id uuid PRIMARY KEY,source_tenant_id uuid NOT NULL,organization_id uuid NOT NULL,event_type text NOT NULL,aggregate_type text NOT NULL,aggregate_id uuid NOT NULL,aggregate_version bigint NOT NULL,event_envelope jsonb NOT NULL,created_at timestamptz NOT NULL DEFAULT now())');
 await db.query(fixtureCompanyRoutingDdl('company_fixture'));
 for(const stream of ['company','audit','delivery']){
  const row=(await db.query("INSERT INTO company_fixture.company_event_outbox(id,source_tenant_id,organization_id,event_type,aggregate_type,aggregate_id,aggregate_version,event_envelope,event_stream) VALUES($1,$2,$3,'fixture.event','employee',$4,1,'{}',$5) RETURNING event_stream",[randomUUID(),tenant,stream==='company'?org:null,principal,stream])).rows[0];assert.equal(row.event_stream,stream);
 }
 for(const [stream,organization] of [['company',null],['audit',org],['unknown',org]])await assert.rejects(db.query("INSERT INTO company_fixture.company_event_outbox(id,source_tenant_id,organization_id,event_type,aggregate_type,aggregate_id,aggregate_version,event_envelope,event_stream) VALUES($1,$2,$3,'fixture.event','employee',$4,1,'{}',$5)",[randomUUID(),tenant,organization,principal,stream]),error=>error.code==='23514');
 await db.query('CREATE SCHEMA kabipay_ops;CREATE TABLE kabipay_ops.organization(id uuid PRIMARY KEY);CREATE TABLE kabipay_ops.tenant(id uuid PRIMARY KEY);CREATE TABLE kabipay_ops.principal(id uuid PRIMARY KEY)');
 await db.query(migration('0113_organization_control_outbox/organization_control_outbox.xml'));
 await db.query(fixtureControlRoutingDdl());
 await db.query('INSERT INTO kabipay_ops.organization VALUES($1)',[org]);await db.query('INSERT INTO kabipay_ops.tenant VALUES($1)',[tenant]);await db.query('INSERT INTO kabipay_ops.principal VALUES($1)',[principal]);
 const rootStore=readFileSync(new URL('../../../crates/kabipay-organization/src/store.rs',import.meta.url),'utf8'),outboxSql=rootStore.match(/"(INSERT INTO kabipay_ops\.organization_outbox\(id,[^"\n]+)"/)[1];
 const payload={actor:{principal_id:principal,source_tenant_id:tenant,source_user_id:user,issuer:'kabipay-client'},configuration_version:1,change:{principal_id:principal,link_id:user,display_name:'Routing fixture',status:'VERIFIED'}};
 await db.query(outboxSql,[eventId,null,tenant,principal,principal,1,'identity.registered',payload]);
 const row=(await db.query('SELECT routed_envelope,routed_envelope::text AS raw,event_stream,source_position FROM kabipay_ops.organization_outbox WHERE id=$1',[eventId])).rows[0];
 assert.equal(row.event_stream,'control');assert.equal(row.source_position,'1');assert.equal(row.routed_envelope.source_database,database);assert.equal(row.routed_envelope.source_schema,'kabipay_ops');assert.equal(Object.keys(row.routed_envelope.event).length,10);assert(!Object.hasOwn(row.routed_envelope.event,'event_stream'));assert(!Object.hasOwn(row.routed_envelope.event,'source_position'));
 const [company,control]=fixtureConnectors({tenant,sourceDatabase:database,metadataDatabase:database}),topic=fixtureControlTopic(control.config),binding={topic,database,schema:'kabipay_ops',connector_name:control.config['topic.prefix']};
 assert.equal(fixtureCompanyTopic(company.config),`organization.company.${tenant}`);
 const routed=parseControlEvent(Buffer.from(row.raw),topic,binding),eventRouterString=parseControlEvent(Buffer.from(JSON.stringify(row.raw)),topic,binding),native=parseControlEvent(Buffer.from(JSON.stringify({op:'c',source:{db:database,schema:'kabipay_ops',table:'organization_outbox',name:binding.connector_name},after:row.routed_envelope.event})),topic,binding);
 assert.equal(routed.organization_id,null);assert.equal(routed.event_id,eventId);assert.equal(routed.content_hash,eventRouterString.content_hash);assert.equal(routed.content_hash,native.content_hash);
 const conflict=structuredClone(row.routed_envelope);conflict.event.payload.change.display_name='Conflicting fixture';assert.notEqual(parseControlEvent(Buffer.from(JSON.stringify(conflict)),topic,binding).content_hash,routed.content_hash);
 console.log(JSON.stringify({code:'ACCEPTANCE_ROUTING_PG_ACCEPTED',company_streams_and_scope_constraints:true,canonical_control_trigger:true,actual_source_store_insert:true,routed_and_historical_dto_identical:true,json_string_wire_supported:true,conflict_detectable:true}));
}finally{
 if(connected)await db.end();
 if(created){assert(/^routing_fixture_[0-9a-f]{32}$/.test(database));await admin.query(`DROP DATABASE ${database} WITH (FORCE)`);}
 await admin.end();
}
