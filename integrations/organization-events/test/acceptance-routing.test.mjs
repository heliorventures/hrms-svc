import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import * as fixture from './kafka-fixture.mjs';
const tenant='00000000-0000-4000-8000-000000000001';
const [company,control]=fixture.fixtureConnectors({tenant,sourceDatabase:'fixture_source'});
test('acceptance topics resolve only checked company and control EventRouter streams',()=>{
 assert.equal(typeof fixture.fixtureCompanyTopic,'function');
 assert.equal(fixture.fixtureCompanyTopic(company.config),`organization.company.${tenant}`);
 assert.equal(fixture.fixtureControlTopic(control.config),'organization.control.metadata');
 assert.equal(fixture.fixtureControlTopic({...control.config,'transforms.outbox.route.topic.replacement':control.config['transforms.outbox.route.topic.replacement']+'.synthetic.0123456789ab'}),'organization.control.metadata.synthetic.0123456789ab');
 for(const change of [{'transforms.outbox.route.by.field':'aggregate_type'},{'transforms.outbox.route.topic.replacement':'organization.company.${routedByValue}'},{'transforms.outbox.table.field.event.payload':'payload'}])assert.throws(()=>fixture.fixtureCompanyTopic({...company.config,...change}),/CANONICAL/);
 assert.throws(()=>fixture.fixtureControlTopic({...control.config,'transforms.outbox.table.field.event.payload':'payload'}),/CANONICAL/);
});
test('synthetic outbox routing DDL comes from the canonical forward migrations',()=>{
 assert.equal(typeof fixture.fixtureCompanyRoutingDdl,'function');
 const companySql=fixture.fixtureCompanyRoutingDdl('company_fixture'),controlSql=fixture.fixtureControlRoutingDdl();
 assert.match(companySql,/ADD COLUMN event_stream text NOT NULL DEFAULT 'company'/);
 assert.match(companySql,/company_private_stream_scope/);assert(!companySql.includes('${schema}'));
 assert.match(controlSql,/ADD COLUMN routed_envelope jsonb/);
 assert.match(controlSql,/CREATE TRIGGER private_control_envelope BEFORE INSERT/);
 assert(!controlSql.includes('CREATE FUNCTION kabipay_ops.private_control_guard'));
 assert.throws(()=>fixture.fixtureCompanyRoutingDdl('public;DROP SCHEMA kabipay_ops'),/SCHEMA/);
});
test('historical acceptance executables use resolved routing and forward outbox DDL',()=>{
 for(const file of ['isolated-cdc.mjs','gap-lifecycle-acceptance.mjs']){
  const source=readFileSync(new URL(file,import.meta.url),'utf8');
  assert.match(source,/topic=fixtureCompanyTopic\(config\)/,file);
  assert.match(source,/query\(fixtureCompanyRoutingDdl\('company_fixture'\)\)/,file);
 }
 assert.match(readFileSync(new URL('isolated-cdc.mjs',import.meta.url),'utf8'),/source_position bigserial NOT NULL/,'standalone CDC fixture must include the canonical0121 source position header');
 const source=readFileSync(new URL('control-cdc-acceptance.mjs',import.meta.url),'utf8');
 assert(!source.includes('transforms.fixedTopic'));
 assert.match(source,/fixtureControlTopic\(config\)/);
 assert.match(source,/query\(fixtureControlRoutingDdl\(\)\)/);
 assert.match(source,/conflict\.event\.payload/);
});
