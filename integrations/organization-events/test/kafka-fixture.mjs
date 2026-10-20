import {connectors,topics} from '../../../../hrms-kafka/scripts/admin.mjs';
import {readFileSync} from 'node:fs';

// Resolve the canonical infrastructure contract before applying only explicit
// local transport settings. Schema, role, slot, table filters and routing remain
// owned by hrms-kafka, so acceptance cannot silently test an obsolete connector.
function fixtureBinding({tenant,sourceDatabase,metadataDatabase=sourceDatabase,sourceSchema='company_fixture'}) {
 return {companyId:tenant,companySchema:sourceSchema,companyDatabase:sourceDatabase,metadataDatabase,companyHost:'postgres',metadataHost:'postgres',connectPrincipal:'User:CN=fixture-connect',inboxPrincipal:'User:CN=fixture-inbox',historyPrincipal:'User:CN=fixture-history',reportingPrincipal:'User:CN=fixture-reporting',capturePrincipal:'User:CN=fixture-capture',controlPrincipal:'User:CN=fixture-control',controlConnectPrincipal:'User:CN=fixture-control-connect',auditPrincipal:'User:CN=fixture-audit',deliveryPrincipal:'User:CN=fixture-delivery',operatorAuditPrincipal:'User:CN=fixture-operator-audit',consumerGroupBase:'fixture-company',controlGroupId:'fixture-control'};
}
export function fixtureConnectors(parameters){return connectors(fixtureBinding(parameters));}
function checkedStreamTopic(config,stream,payload,pattern){
 if(config['transforms.outbox.type']!=='io.debezium.transforms.outbox.EventRouter'||config['transforms.outbox.route.by.field']!=='event_stream'||config['transforms.outbox.table.field.event.payload']!==payload)throw Error('CANONICAL_STREAM_ROUTER_REQUIRED');
 const template=config['transforms.outbox.route.topic.replacement'];
 if(typeof template!=='string'||!pattern.test(template))throw Error('CANONICAL_STREAM_TOPIC_REQUIRED');
 return template.replace('${routedByValue}',stream);
}
export function fixtureCompanyTopic(config){return checkedStreamTopic(config,'company','event_envelope',/^organization\.\$\{routedByValue\}\.[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);}
export function fixtureControlTopic(config){return checkedStreamTopic(config,'control','routed_envelope',/^organization\.\$\{routedByValue\}\.metadata(?:\.synthetic\.[0-9a-f]{12})?$/);}
function forwardRoutingSql(file,boundary){
 const xml=readFileSync(new URL('../../../../hrms-database/changelog/migrations/'+file,import.meta.url),'utf8'),sql=xml.match(/<!\[CDATA\[([\s\S]*?)\]\]>/)?.[1],end=sql?.indexOf(boundary);
 if(!sql||end<0)throw Error('CANONICAL_OUTBOX_ROUTING_DDL_REQUIRED');return sql.slice(0,end);
}
// Historical acceptance intentionally installs old business migrations. Apply
// only the current stream columns/envelope trigger, straight from forward DDL.
export function fixtureCompanyRoutingDdl(schema){
 if(typeof schema!=='string'||!/^company_[A-Za-z0-9_]{1,55}$/.test(schema))throw Error('SYNTHETIC_ROUTING_SCHEMA');
 return forwardRoutingSql('0125_private_event_source/private_event_source.xml','CREATE INDEX private_outbox_pending').replaceAll('${schema}',schema);
}
export function fixtureControlRoutingDdl(){return forwardRoutingSql('0124_private_event_control/private_event_control.xml','CREATE FUNCTION kabipay_ops.private_control_guard()');}
export function connectorHeartbeatTopic(config){
 if(typeof config['topic.heartbeat.prefix']!=='string'||!config['topic.heartbeat.prefix']||typeof config['topic.prefix']!=='string'||!config['topic.prefix'])throw Error('CANONICAL_HEARTBEAT_TOPIC_REQUIRED');
 return config['topic.heartbeat.prefix']+'.'+config['topic.prefix'];
}
export function fixtureCompanyTopics(parameters){
 const binding=fixtureBinding(parameters),config=connectors(binding)[0].config;
 // These existing fixtures publish only the checked company discriminator;
 // the separate private CDC fixture exercises audit/delivery/operator routing.
 const companyTopic=fixtureCompanyTopic(config);
 const required=[companyTopic,`${config['topic.prefix']}.${binding.companySchema}.organization_capture_marker`,connectorHeartbeatTopic(config)];
 const managed=topics(binding);if(required.some(topic=>!managed.includes(topic)))throw Error('CANONICAL_FIXTURE_TOPIC_MISMATCH');return required;
}
export function isolatedConnector(connector,password) {
 const config={...connector.config,'database.password':password,'database.sslmode':'disable'};
 delete config['database.sslrootcert'];
 return {...connector,config};
}
export async function withFixtureCleanup(run,cleanup){
 let primaryFailure,failed=false;
 try{return await run();}catch(error){failed=true;primaryFailure=error;throw error;}
 finally{try{await cleanup();}catch(cleanupFailure){if(failed)throw new AggregateError([primaryFailure,cleanupFailure],'SYNTHETIC_PRIMARY_AND_CLEANUP_FAILURE');throw cleanupFailure;}}
}
