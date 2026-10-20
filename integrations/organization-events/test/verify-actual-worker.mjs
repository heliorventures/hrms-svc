import assert from 'node:assert/strict';
import {Kafka,logLevel} from 'kafkajs';
if(process.env.ORGANIZATION_SYNTHETIC_ACCEPTANCE!=='true')throw Error('EXPLICIT_SYNTHETIC_OPT_IN_REQUIRED');
const kafka=new Kafka({clientId:'synthetic-worker-verifier',brokers:['127.0.0.1:19092'],logLevel:logLevel.NOTHING});
const admin=kafka.admin();const consumer=kafka.consumer({groupId:'synthetic-quarantine-check-'+Date.now()});
let timer;
try {
 await admin.connect();const offsets=await admin.fetchOffsets({groupId:'organization-actual-worker-acceptance',topics:['organization.synthetic.acceptance']});assert.equal(offsets[0].partitions[0].offset,'6');
 await consumer.connect();await consumer.subscribe({topic:'organization.synthetic.quarantine',fromBeginning:true});
 let resolve,reject;const done=new Promise((r,j)=>{resolve=r;reject=j;});timer=setTimeout(()=>reject(Error('DLQ_TIMEOUT')),30000);
 await consumer.run({eachMessage:async({message})=>{try{if(message.key?.toString()==='probe')return;const m=JSON.parse(message.value.toString());assert.equal(m.code,'FIELD_SET');assert.equal(m.offset,'5');assert.equal(Object.hasOwn(m,'payload'),false);assert(!message.value.toString().includes('private-do-not-copy'));resolve();}catch(e){reject(e);}}});
 await done;console.log(JSON.stringify({result:'PASS',actual_worker_next_offset:6,actual_kafka_quarantine_metadata_only:true}));
}finally{clearTimeout(timer);await consumer.disconnect();await admin.disconnect();}
