import test from 'node:test';
import assert from 'node:assert/strict';
import {fixtureConnectors,fixtureCompanyTopics,connectorHeartbeatTopic,isolatedConnector,withFixtureCleanup} from './kafka-fixture.mjs';
const tenant='00000000-0000-4000-8000-000000000001',other='00000000-0000-7000-8000-000000000002';

test('acceptance uses canonical nonpublic company schema and company-specific CDC identities',()=>{
 const first=fixtureConnectors({tenant,sourceDatabase:'fixture_source',metadataDatabase:'fixture_metadata'}),second=fixtureConnectors({tenant:other,sourceDatabase:'other_source',sourceSchema:'company_two'});
 for(const [id,rendered,schema] of [[tenant,first[0],'company_fixture'],[other,second[0],'company_two']]) {
  const config=rendered.config;
  assert.equal(config['database.user'],'organization_cdc_'+id.replaceAll('-',''));
  assert.equal(config['slot.name'],'organization_company_'+id.replaceAll('-',''));
  assert(new RegExp(config['table.include.list'].split(',')[0]+'$').test(schema+'.company_event_outbox'));
  assert(new RegExp(config['table.include.list'].split(',')[1]+'$').test(schema+'.organization_capture_marker'));
  assert(!new RegExp(config['table.include.list'].split(',')[0]+'$').test('public.company_event_outbox'));
  assert(new RegExp('^'+config['predicates.isOutbox.pattern']+'$').test(config['topic.prefix']+'.'+schema+'.company_event_outbox'));
  assert(!new RegExp('^'+config['predicates.isOutbox.pattern']+'$').test(config['topic.prefix']+'.'+schema+'.organization_capture_marker'));
  assert(!JSON.stringify(rendered).includes('REPLACE_'));
 }
 assert.notEqual(first[0].config['slot.name'],second[0].config['slot.name']);
 assert.equal(first[1].config['database.dbname'],'fixture_metadata');
 assert.equal(first[1].config['database.user'],'organization_control_cdc');
 assert.throws(()=>fixtureConnectors({tenant,sourceDatabase:'fixture_source',sourceSchema:'public'}),/companySchema/);
});

test('isolated transport leaves canonical role, slot, schema and SMT contracts intact',()=>{
 const original=fixtureConnectors({tenant,sourceDatabase:'fixture_source'})[0],local=isolatedConnector(original,'synthetic-only');
 assert.equal(local.config['database.sslmode'],'disable');
 assert.equal(original.config['database.sslmode'],'verify-full');
 for(const key of ['database.user','slot.name','publication.name','topic.prefix','table.include.list','predicates.isOutbox.pattern','transforms.outbox.table.expand.json.payload','transforms.outbox.route.topic.replacement'])assert.equal(local.config[key],original.config[key]);
});

test('precreated streaming topics include the canonical company and control heartbeat contracts',()=>{
 const parameters={tenant,sourceDatabase:'fixture_source'},[company,control]=fixtureConnectors(parameters),planned=fixtureCompanyTopics(parameters);
 assert.deepEqual(planned,[`organization.company.${tenant}`,company.config['topic.prefix']+'.company_fixture.organization_capture_marker',connectorHeartbeatTopic(company.config)]);
 assert.equal(company.config['transforms.outbox.route.by.field'],'event_stream');
 assert.equal(planned[2],'organization.heartbeat.organization.company.'+tenant);
 assert.equal(connectorHeartbeatTopic(control.config),'organization.heartbeat.organization_control_metadata');
 assert.throws(()=>connectorHeartbeatTopic({'topic.prefix':'fixed'}),/CANONICAL_HEARTBEAT_TOPIC_REQUIRED/);
});

test('cleanup preserves the original failure and reports both causes when cleanup also fails',async()=>{
 const primary=Error('CDC_TIMEOUT'),cleanup=Error('SLOT_STILL_ACTIVE');let cleaned=false;
 await assert.rejects(withFixtureCleanup(async()=>{throw primary;},async()=>{cleaned=true;}),error=>error===primary);assert(cleaned);
 await assert.rejects(withFixtureCleanup(async()=>{throw primary;},async()=>{throw cleanup;}),error=>error instanceof AggregateError&&error.errors[0]===primary&&error.errors[1]===cleanup);
 await assert.rejects(withFixtureCleanup(async()=>{throw null;},async()=>{throw cleanup;}),error=>error instanceof AggregateError&&error.errors[0]===null&&error.errors[1]===cleanup);
 await assert.rejects(withFixtureCleanup(async()=>1,async()=>{throw cleanup;}),error=>error===cleanup);
 assert.equal(await withFixtureCleanup(async()=>7,async()=>{}),7);
});
