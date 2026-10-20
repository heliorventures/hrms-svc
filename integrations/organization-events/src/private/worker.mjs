import {readFileSync} from 'node:fs';
import {randomUUID} from 'node:crypto';
import {PrivateClickHouse} from './clickhouse.mjs';
import {PrivateHistoryProcessor,privateBinding} from './history.mjs';
import {parseKeyring,encryptFrames} from './crypto.mjs';
import {newWork,DeliveryCoordinator,SourceDeliveryClient} from './delivery.mjs';
import {required,jsonFile,kafkaClient,requireSinglePartitions,positionHeader,log} from './runtime.mjs';
import {uuid} from './contract.mjs';
const binding=privateBinding(jsonFile('PRIVATE_EVENT_BINDING_FILE'));
const keys=parseKeyring(readFileSync(required('KABIPAY_PRIVATE_EVENT_KEYS_FILE'),'utf8'));
for(const id of keys.keys.keys())if(!id.startsWith(`${binding.tenant_id}/${binding.purpose}/`))throw Error('PRIVATE_KEY_SCOPE_EXCESS');
const store=new PrivateClickHouse({url:required('CLICKHOUSE_URL'),user:required('CLICKHOUSE_USER'),password:required('CLICKHOUSE_PASSWORD'),purpose:binding.purpose,isolated:process.env.ALLOW_ISOLATED_PLAINTEXT==='true'});
const processor=new PrivateHistoryProcessor({store,keys,binding}),delivery=binding.purpose==='delivery';
const prefix='organization.delivery',suffix=binding.tenant_id;
const topics=delivery?[binding.topic,`${prefix}.history.${suffix}`,`${prefix}.work.${suffix}`,...[1,2,3].map(n=>`${prefix}.retry.${n}.${suffix}`)]:[binding.topic];
const groupId=required('PRIVATE_EVENT_GROUP_ID');if(!/^[A-Za-z0-9_.-]{1,200}$/.test(groupId))throw Error('PRIVATE_GROUP_ID');
const kafka=kafkaClient(`private-${binding.purpose}-${binding.tenant_id}`),consumer=kafka.consumer({groupId,allowAutoTopicCreation:false,maxBytesPerPartition:1048576,sessionTimeout:60000}),producer=delivery?kafka.producer({allowAutoTopicCreation:false,idempotent:true,maxInFlightRequests:1}):null;
let coordinator,outcomes,sourceClient,brokerOwner;
if(delivery){
 const config=jsonFile('KABIPAY_PRIVATE_DELIVERY_WORKER_FILE');
 if(config.schema_version!==1||!Array.isArray(config.bindings)||config.bindings.length!==1)throw Error('PRIVATE_DELIVERY_BINDING');
 const source=config.bindings[0],key=Buffer.from(source.key_b64??'','base64');
 if(source.tenant_id!==binding.tenant_id||source.source_generation!==binding.source_generation||key.toString('base64')!==source.key_b64)throw Error('PRIVATE_DELIVERY_BINDING');
 const optional=name=>process.env[name]?readFileSync(process.env[name]):undefined;
 brokerOwner=required('PRIVATE_DELIVERY_BROKER_OWNER_ID');if(!uuid(brokerOwner))throw Error('PRIVATE_DELIVERY_OWNER');
 sourceClient=new SourceDeliveryClient({url:source.url,key,ca:optional('PRIVATE_SOURCE_CA_FILE'),cert:optional('PRIVATE_SOURCE_CERT_FILE'),privateKey:optional('PRIVATE_SOURCE_KEY_FILE')});
 coordinator=new DeliveryCoordinator({client:sourceClient,delays:config.retry_delays_seconds??[30,300,3600],probeDelay:config.probe_delay_seconds??15,maxProbes:config.max_probes??8});
 outcomes=new PrivateHistoryProcessor({store,keys,binding:{...binding,mode:'outcome',topic:`${prefix}.history.${suffix}`}});
}
const timers=new Set();let stopping=false;
async function stop(){if(stopping)return;stopping=true;for(const timer of timers)clearTimeout(timer);await consumer.stop();await consumer.disconnect();if(producer)await producer.disconnect();}
for(const signal of ['SIGTERM','SIGINT'])process.once(signal,()=>stop().catch(()=>process.exitCode=1));
consumer.on(consumer.events.CRASH,()=>{log('PRIVATE_CONSUMER_CRASH');stop().finally(()=>process.exitCode=1);});
async function publish(topic,item){return producer.send({topic,acks:-1,messages:[{key:item.ref?.intent_id??item.record_id,value:JSON.stringify(item)}]});}
async function recordOutcome(item,result){
 const id=randomUUID(),occurred=new Date().toISOString().replace(/(\.\d{3})Z$/,'$1000Z');
 const raw=Buffer.from(JSON.stringify({id,tenant_id:binding.tenant_id,intent_id:item.ref.intent_id,destination_id:item.ref.destination_id,source_generation:binding.source_generation,attempt:item.attempt,lease_generation:item.lease_generation,action:result.kind,outcome_code:result.code??result.response?.outcome_code??'none',response:result.response??null,created_at:occurred}));
 const frames=encryptFrames(raw,{record_id:id,tenant_id:binding.tenant_id,purpose:'delivery',source_generation:binding.source_generation,source_kind:'delivery_outcome',resource_type:'delivery_attempt',resource_id:item.ref.intent_id,occurred_at:occurred},keys);
 await producer.send({topic:`${prefix}.history.${suffix}`,acks:-1,messages:frames.map(frame=>({key:id,value:JSON.stringify(frame),headers:{source_position:'0'}}))});
}
await requireSinglePartitions(kafka,delivery?[...topics,`${prefix}.dlq.${suffix}`]:topics);
if(producer)await producer.connect();await consumer.connect();await consumer.subscribe({topics,fromBeginning:true});
await consumer.run({autoCommit:false,eachBatchAutoResolve:false,partitionsConsumedConcurrently:1,eachBatch:async({batch,resolveOffset,heartbeat,isRunning,isStale,pause})=>{
 for(const message of batch.messages){
  if(stopping||!isRunning()||isStale())break;
  const position={topic:batch.topic,partition:batch.partition,offset:message.offset};
  try{
   if(batch.topic===binding.topic){
    const result=await processor.accept({...position,raw:message.value,sourcePosition:positionHeader(message)});
    // Work is durable in Kafka before the source offset can be acknowledged.
    if(result.delivery){
     const topic=`${prefix}.work.${suffix}`,acks=await publish(topic,newWork(result.delivery,Math.floor(Date.now()/1000),result.workOptions??{}));
     if(acks.length!==1||acks[0].partition!==0||typeof acks[0].baseOffset!=='string')throw Error('PRIVATE_BROKER_RECEIPT');
     await sourceClient.own({version:1,...result.delivery,broker_owner_id:brokerOwner,topic,partition_id:0,kafka_offset:acks[0].baseOffset,request_id:randomUUID(),issued_at:Math.floor(Date.now()/1000)});
    }
   }else if(batch.topic===`${prefix}.history.${suffix}`){
    await outcomes.accept({...position,raw:message.value,sourcePosition:positionHeader(message)});
   }else{
    if(!message.value||message.value.length>8192)throw Error('PRIVATE_WORK_SIZE');const item=JSON.parse(message.value);
    if(item.ref?.tenant_id!==binding.tenant_id||item.ref?.source_generation!==binding.source_generation)throw Error('PRIVATE_WORK_SCOPE');
    const result=await coordinator.run(item);
    if(result.kind==='wait'){
     const resume=pause();
     const timer=setTimeout(()=>{timers.delete(timer);if(!stopping)resume();},result.waitMs);timers.add(timer);break;
    }
    if(result.kind==='retry')await publish(`${prefix}.retry.${result.level}.${suffix}`,result.next);
    else if(result.kind==='dlq')await publish(`${prefix}.dlq.${suffix}`,{...item,disposition:result.code,quarantined_at:Math.floor(Date.now()/1000)});
    await recordOutcome(item,result);
   }
   resolveOffset(message.offset);await consumer.commitOffsets([{topic:batch.topic,partition:batch.partition,offset:(BigInt(message.offset)+1n).toString()}]);await heartbeat();log('PRIVATE_RECORD_OWNED',position);
  }catch{log('PRIVATE_RECORD_RETRY',position);throw Error('PRIVATE_RECORD_RETRY');}
 }
}});
