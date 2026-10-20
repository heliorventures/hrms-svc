// Separate raw-CDC coordinator. No source DB credentials and no business body.
import {readFileSync} from 'node:fs';
import {Kafka,logLevel} from 'kafkajs';
import pg from 'pg';
import {coordinateMarker} from './capture.mjs';
import {validateCaptureBindings} from './capture-bindings.mjs';
import {connectorConfirmation} from './connect-checkpoint.mjs';
const required=k=>{if(!process.env[k])throw Error(`MISSING_${k}`);return process.env[k];};
const isolated=process.env.ALLOW_ISOLATED_PLAINTEXT==='true';
const bindings=validateCaptureBindings(JSON.parse(readFileSync(required('CAPTURE_BINDINGS_FILE'),'utf8')),{isolated});
const kafka=new Kafka({clientId:'organization-capture',brokers:required('KAFKA_BROKERS').split(','),logLevel:logLevel.NOTHING,ssl:isolated?false:{ca:[readFileSync(required('KAFKA_CA_FILE'),'utf8')],cert:readFileSync(required('KAFKA_CERT_FILE'),'utf8'),key:readFileSync(required('KAFKA_KEY_FILE'),'utf8'),rejectUnauthorized:true}});
const pool=new pg.Pool({connectionString:required('ORGANIZATION_METADATA_DATABASE_URL'),max:2,ssl:isolated?false:{ca:readFileSync(required('POSTGRES_CA_FILE'),'utf8'),rejectUnauthorized:true},statement_timeout:30000});
const consumer=kafka.consumer({groupId:required('KAFKA_GROUP_ID')+'.capture.v2',allowAutoTopicCreation:false,sessionTimeout:120000});
const producer=kafka.producer({idempotent:true,maxInFlightRequests:1,allowAutoTopicCreation:false});
const admin=kafka.admin();await admin.connect();
try{for(const b of Object.values(bindings)){const metadata=await admin.fetchTopicMetadata({topics:[b.company_topic]});const actual=metadata.topics[0]?.partitions.map(p=>p.partitionId).sort((a,b)=>a-b);if(JSON.stringify(actual)!==JSON.stringify([...b.expected_partitions].sort((a,b)=>a-b)))throw Error('CAPTURE_PARTITION_CONFIGURATION_MISMATCH');}}finally{await admin.disconnect();}
let stopping=false;async function stop(){if(stopping)return;stopping=true;await consumer.stop();await consumer.disconnect();await producer.disconnect();await pool.end();}
process.once('SIGINT',()=>stop());process.once('SIGTERM',()=>stop());
consumer.on(consumer.events.CRASH,()=>{console.error(JSON.stringify({level:'error',code:'CAPTURE_COORDINATOR_RETRY'}));stop().finally(()=>process.exit(1));});
await producer.connect();await consumer.connect();await consumer.subscribe({topics:Object.keys(bindings),fromBeginning:true});
await consumer.run({autoCommit:false,eachMessage:async({topic,partition,message,heartbeat})=>{
 const binding=bindings[topic];let snapshot=false;try{snapshot=JSON.parse(message.value).op==='r';}catch{}
 if(!snapshot)await coordinateMarker({raw:message.value??Buffer.from('null'),binding,pool,producer,quarantine:metadata=>producer.send({topic:required('KAFKA_QUARANTINE_TOPIC'),acks:-1,messages:[{key:`${topic}:${partition}:${message.offset}`,value:JSON.stringify({...metadata,topic,partition,offset:message.offset,role:'capture'})}]}),confirmCapture:connectorConfirmation({url:binding.connect_rest_url,isolated,heartbeat,tls:isolated?{}:{ca:readFileSync(required('CONNECT_CA_FILE')),cert:readFileSync(required('CONNECT_CLIENT_CERT_FILE')),key:readFileSync(required('CONNECT_CLIENT_KEY_FILE'))}})});
 // Snapshot markers are retained source metadata but cannot attest current CDC.
 await consumer.commitOffsets([{topic,partition,offset:(BigInt(message.offset)+1n).toString()}]);}});
