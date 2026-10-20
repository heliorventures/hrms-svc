import {readFileSync} from 'node:fs';
import {Kafka,logLevel} from 'kafkajs';
export const required=name=>{if(!process.env[name])throw Error(`MISSING_${name}`);return process.env[name];};
export function jsonFile(name,max=65536){const bytes=readFileSync(required(name));if(bytes.length>max)throw Error('PRIVATE_CONFIG_SIZE');return JSON.parse(bytes);}
export function kafkaClient(clientId){
 const brokers=required('KAFKA_BROKERS').split(',');
 const isolated=process.env.ALLOW_ISOLATED_PLAINTEXT==='true';
 if(isolated&&brokers.some(b=>!/^(127\.0\.0\.1|localhost|kafka):\d{2,5}$/.test(b)))throw Error('PRIVATE_KAFKA_TLS_REQUIRED');
 const ssl=isolated?false:{ca:[readFileSync(required('KAFKA_CA_FILE'),'utf8')],cert:readFileSync(required('KAFKA_CERT_FILE'),'utf8'),key:readFileSync(required('KAFKA_KEY_FILE'),'utf8'),rejectUnauthorized:true,minVersion:'TLSv1.2'};
 return new Kafka({clientId,brokers,ssl,logLevel:logLevel.NOTHING,allowAutoTopicCreation:false,retry:{retries:8,initialRetryTime:300,maxRetryTime:30000}});
}
export async function requireSinglePartitions(kafka,topics){const admin=kafka.admin();await admin.connect();try{const metadata=await admin.fetchTopicMetadata({topics});if(metadata.topics.length!==topics.length||metadata.topics.some(t=>t.partitions.length!==1||t.partitions[0].partitionId!==0))throw Error('PRIVATE_SINGLE_PARTITION_REQUIRED');}finally{await admin.disconnect();}}
export function positionHeader(message){const value=message.headers?.source_position;if(!Buffer.isBuffer(value)||!/^[0-9]{1,19}$/.test(value.toString('utf8')))throw Error('PRIVATE_SOURCE_POSITION_REQUIRED');return value.toString('utf8');}
export function log(code,position={}){console.log(JSON.stringify({level:'info',code,...position,time:new Date().toISOString()}));}
