import {readFileSync} from 'node:fs';
import {Kafka,logLevel} from 'kafkajs';
import {ControlHttp} from './control-http.mjs';
import {ControlHistory} from './control-history.mjs';
import {processControlRecord} from './control-events.mjs';
const required=k=>{if(!process.env[k])throw Error(`MISSING_${k}`);return process.env[k];};
const bounded=(k,value,min,max)=>{const n=Number(process.env[k]??value);if(!Number.isInteger(n)||n<min||n>max)throw Error(`INVALID_${k}`);return n;};
const binding=JSON.parse(readFileSync(required('CONTROL_BINDING_FILE'),'utf8'));
for(const key of ['topic','database','schema','connector_name'])if(typeof binding[key]!=='string'||!binding[key].length||binding[key].length>200||!/^[a-zA-Z0-9_.-]+$/.test(binding[key]))throw Error('INVALID_CONTROL_BINDING');
const quarantineTopic=required('CONTROL_QUARANTINE_TOPIC');
if(quarantineTopic===binding.topic)throw Error('CONTROL_QUARANTINE_TOPIC_COLLISION');
const isolated=process.env.ALLOW_ISOLATED_PLAINTEXT==='true';
const ssl=isolated?false:{ca:[readFileSync(required('KAFKA_CA_FILE'),'utf8')],cert:readFileSync(required('KAFKA_CERT_FILE'),'utf8'),key:readFileSync(required('KAFKA_KEY_FILE'),'utf8'),rejectUnauthorized:true};
const kafka=new Kafka({clientId:'organization-control-history',brokers:required('KAFKA_BROKERS').split(','),ssl,logLevel:logLevel.NOTHING,sasl:process.env.KAFKA_SASL==='true'?{mechanism:'scram-sha-512',username:required('KAFKA_USERNAME'),password:required('KAFKA_PASSWORD')}:undefined,retry:{retries:bounded('KAFKA_RETRIES',8,1,20),initialRetryTime:bounded('KAFKA_RETRY_INITIAL_MS',300,100,10000),maxRetryTime:bounded('KAFKA_RETRY_MAX_MS',30000,1000,60000)}});
// Deliberately no PostgreSQL dependency or connection: only metadata CDC connector
// owns restricted fixed metadata DB credentials; this consumer sees broker+CH.
const history=new ControlHistory(new ControlHttp({url:required('CLICKHOUSE_URL'),user:required('CLICKHOUSE_USER'),password:required('CLICKHOUSE_PASSWORD')}));
const consumer=kafka.consumer({groupId:required('CONTROL_GROUP_ID'),allowAutoTopicCreation:false,maxBytesPerPartition:1048576,sessionTimeout:60000});
const producer=kafka.producer({allowAutoTopicCreation:false,idempotent:true,maxInFlightRequests:1});
let stopping=false;
async function stop(){if(stopping)return;stopping=true;await consumer.stop();await consumer.disconnect();await producer.disconnect();}
for(const signal of ['SIGINT','SIGTERM'])process.once(signal,()=>stop().catch(()=>process.exitCode=1));
consumer.on(consumer.events.CRASH,()=>{console.error(JSON.stringify({level:'error',code:'CONTROL_CONSUMER_CRASH',time:new Date().toISOString()}));stop().finally(()=>process.exit(1));});
await producer.connect();await consumer.connect();await consumer.subscribe({topic:binding.topic,fromBeginning:true});
// One ordered metadata partition is intentional: conflict checks and writes
// serialize globally. Repartitioning requires a reviewed keyed conflict fence.
const admin=kafka.admin();await admin.connect();
try{const metadata=await admin.fetchTopicMetadata({topics:[binding.topic]});if(metadata.topics.length!==1||metadata.topics[0].partitions.length!==1)throw Error('CONTROL_SINGLE_PARTITION_REQUIRED');}finally{await admin.disconnect();}
await consumer.run({autoCommit:false,eachBatchAutoResolve:false,partitionsConsumedConcurrently:1,eachBatch:async({batch,resolveOffset,heartbeat,isRunning,isStale})=>{
 for(const message of batch.messages){
  if(stopping||!isRunning()||isStale())break;
  const position={topic:batch.topic,partition:batch.partition,offset:message.offset};
  try{
   const result=await processControlRecord({...position,raw:message.value??Buffer.from('null'),binding,history,quarantine:metadata=>producer.send({topic:quarantineTopic,acks:-1,messages:[{key:`${position.topic}:${position.partition}:${position.offset}`,value:JSON.stringify(metadata)}]})});
   resolveOffset(message.offset);
   await consumer.commitOffsets([{topic:batch.topic,partition:batch.partition,offset:(BigInt(message.offset)+1n).toString()}]);
   console.log(JSON.stringify({level:'info',code:result.quarantined?'CONTROL_QUARANTINED':'CONTROL_APPLIED',...position,time:new Date().toISOString()}));
   await heartbeat();
  }catch(error){console.error(JSON.stringify({level:'error',code:'CONTROL_RETRY',...position,time:new Date().toISOString()}));throw error;}
 }
}});
