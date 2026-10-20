import {readFileSync} from 'node:fs';
import {Kafka,logLevel} from 'kafkajs';
import pg from 'pg';
import {processConsumerRecord,consumerGroup} from './consumers.mjs';
import {ProjectionStore} from './store.mjs';
import {ClickHouseHistory} from './history.mjs';
import {diagnosticsFromEnvironment} from './diagnostics.mjs';
import {parseBarrier} from './capture.mjs';
function required(key) { const value=process.env[key]; if(!value) throw new Error(`MISSING_${key}`); return value; }
function bounded(key,fallback,min,max) {const value=Number(process.env[key]??fallback);if(!Number.isInteger(value)||value<min||value>max)throw new Error(`INVALID_${key}`);return value;}
const bindings=JSON.parse(readFileSync(required('TOPIC_BINDINGS_FILE'),'utf8'));
const role=required('CONSUMER_ROLE');
const groupId=consumerGroup(required('KAFKA_GROUP_ID'),role);
if(!Object.keys(bindings).length) throw new Error('NO_TOPICS');
const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
for(const [topic,binding] of Object.entries(bindings)) {
 const organizations=binding.organization_ids??[binding.organization_id];
 if(!/^[a-zA-Z0-9._-]+$/.test(topic)||!uuid.test(binding.tenant_id)||!Array.isArray(organizations)||organizations.length===0||!organizations.every(id=>typeof id==='string'&&uuid.test(id)))throw new Error('INVALID_TOPIC_BINDING');
}
const isolated=process.env.ALLOW_ISOLATED_PLAINTEXT==='true';
const ssl=isolated?false:{ca:[readFileSync(required('KAFKA_CA_FILE'),'utf8')],cert:readFileSync(required('KAFKA_CERT_FILE'),'utf8'),key:readFileSync(required('KAFKA_KEY_FILE'),'utf8'),rejectUnauthorized:true};
const kafka=new Kafka({clientId:'organization-projections',brokers:required('KAFKA_BROKERS').split(','),ssl,
  sasl:process.env.KAFKA_SASL==='true'?{mechanism:'scram-sha-512',username:required('KAFKA_USERNAME'),password:required('KAFKA_PASSWORD')}:undefined,
  logLevel:logLevel.NOTHING,retry:{retries:bounded('KAFKA_RETRIES',8,1,20),initialRetryTime:bounded('KAFKA_RETRY_INITIAL_MS',300,100,10000),maxRetryTime:bounded('KAFKA_RETRY_MAX_MS',30000,1000,60000)}});
// Never accept tenant DB connection strings. This credential has only projection
// table privileges in the organization metadata database (see grants.sql).
const pool=new pg.Pool({connectionString:required('ORGANIZATION_METADATA_DATABASE_URL'),max:4,
  ssl:isolated?false:{ca:readFileSync(required('POSTGRES_CA_FILE'),'utf8'),rejectUnauthorized:true},statement_timeout:30000});
const store=new ProjectionStore(pool);
const diagnostics=diagnosticsFromEnvironment();
const history=role==='inbox'?null:new ClickHouseHistory({url:required('CLICKHOUSE_URL'),user:required('CLICKHOUSE_USER'),password:required('CLICKHOUSE_PASSWORD'),table:role==='history'?'business_history':'reporting_history'});
const consumer=kafka.consumer({groupId,allowAutoTopicCreation:false,maxBytesPerPartition:1048576,sessionTimeout:bounded('KAFKA_SESSION_TIMEOUT_MS',60000,10000,120000)});
const producer=kafka.producer({allowAutoTopicCreation:false,idempotent:true,maxInFlightRequests:1,retry:{retries:bounded('KAFKA_RETRIES',8,1,20)}});
let stopping=false;
async function shutdown() { if(stopping)return; stopping=true; await consumer.stop(); await consumer.disconnect(); await producer.disconnect(); await pool.end(); }
process.once('SIGINT',()=>shutdown().catch(()=>process.exitCode=1));
process.once('SIGTERM',()=>shutdown().catch(()=>process.exitCode=1));
consumer.on(consumer.events.CRASH,()=>{diagnostics.emit('CONSUMER_CRASH');console.error(JSON.stringify({level:'error',code:'CONSUMER_CRASH',time:new Date().toISOString()}));shutdown().finally(()=>process.exit(1));});
await producer.connect(); await consumer.connect();
await consumer.subscribe({topics:Object.keys(bindings),fromBeginning:true});
await consumer.run({autoCommit:false,eachBatchAutoResolve:false,partitionsConsumedConcurrently:1,
  eachBatch:async ({batch,resolveOffset,heartbeat,isRunning,isStale})=>{
    for(const message of batch.messages) {
      if(stopping||!isRunning()||isStale())break;
      const context={topic:batch.topic,partition:batch.partition,offset:message.offset};
      try {
        const raw=message.value??Buffer.from('null');
        let barrier=false;try{barrier=JSON.parse(raw.toString()).event_type==='capture.barrier';}catch{}
        let result;
        if(barrier){
          const e=parseBarrier(raw);const b=bindings[batch.topic];
          if(b.tenant_id!==e.source_tenant_id||!(b.organization_ids??[b.organization_id]).includes(e.organization_id))throw Error('BARRIER_PROVENANCE');
          if(history)await history.query('SELECT 1');
          result=await store.barrier(e,{...context,role})??{quarantined:false};
        }else result=await processConsumerRecord({...context,role,raw,bindings,store,history,
          quarantine:async metadata=>producer.send({topic:required('KAFKA_QUARANTINE_TOPIC'),messages:[{key:`${context.topic}:${context.partition}:${context.offset}`,value:JSON.stringify(metadata)}],acks:-1})});
        resolveOffset(message.offset);
        await consumer.commitOffsets([{topic:batch.topic,partition:batch.partition,offset:(BigInt(message.offset)+1n).toString()}]);
        diagnostics.emit(result.quarantined?'PROJECTION_QUARANTINED':result.historical?'HISTORY_RETAINED':'PROJECTION_APPLIED',context);
        await heartbeat();
      } catch(error) {
        // No raw input, token, connection string or arbitrary exception text.
        console.error(JSON.stringify({level:'error',code:'PROJECTION_RETRY',...context,time:new Date().toISOString()}));
        diagnostics.emit('PROJECTION_RETRY',context);
        throw error;
      }
    }
  }});
