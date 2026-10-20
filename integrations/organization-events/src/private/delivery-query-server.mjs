import {readFileSync} from 'node:fs';
import {createServer} from 'node:https';
import {PrivateClickHouse} from './clickhouse.mjs';
import {DeliveryHistoryQuery,DeliveryHistoryStore,deliveryHistoryHandler} from './delivery_history.mjs';
const required=n=>{if(!process.env[n])throw Error(`MISSING_${n}`);return process.env[n];};
const file=(n,max)=>{const bytes=readFileSync(required(n));if(!bytes.length||bytes.length>max)throw Error('PRIVATE_HISTORY_CONFIG_SIZE');return bytes;};
const config=JSON.parse(file('KABIPAY_PRIVATE_DELIVERY_HISTORY_SERVER_FILE',65536));if(config.schema_version!==1)throw Error('PRIVATE_HISTORY_CONFIG');
const db=new PrivateClickHouse({url:required('CLICKHOUSE_URL'),user:required('CLICKHOUSE_USER'),password:required('CLICKHOUSE_PASSWORD'),purpose:'delivery'});
const service=new DeliveryHistoryQuery({bindings:config.bindings,store:new DeliveryHistoryStore(db)});
// A fixed purpose process receives only protected delivery SELECT credentials.
const server=createServer({key:file('PRIVATE_HISTORY_TLS_KEY_FILE',65536),cert:file('PRIVATE_HISTORY_TLS_CERT_FILE',131072),ca:file('PRIVATE_HISTORY_CLIENT_CA_FILE',1048576),requestCert:true,rejectUnauthorized:true,minVersion:'TLSv1.2'},deliveryHistoryHandler(service));
const port=Number(process.env.PRIVATE_DELIVERY_HISTORY_PORT??8446);if(!Number.isInteger(port)||port<1024||port>65535)throw Error('PRIVATE_HISTORY_PORT');
server.requestTimeout=30000;server.headersTimeout=10000;server.maxRequestsPerSocket=20;server.maxConnections=32;server.listen(port,'0.0.0.0');for(const signal of ['SIGINT','SIGTERM'])process.once(signal,()=>server.close());
