import {readFileSync} from 'node:fs';
import {createServer as httpsServer} from 'node:https';
import {PrivateClickHouse} from './clickhouse.mjs';
import {PrivateHistoryQuery,privateHistoryHandler} from './query.mjs';
const required=name=>{if(!process.env[name])throw Error(`MISSING_${name}`);return process.env[name];};
const boundedFile=(name,max)=>{const bytes=readFileSync(required(name));if(bytes.length>max)throw Error('PRIVATE_HISTORY_CONFIG_SIZE');return bytes;};
const configuration=JSON.parse(boundedFile('KABIPAY_PRIVATE_HISTORY_SERVER_FILE',65536));
if(configuration.schema_version!==1||!Array.isArray(configuration.bindings)||configuration.bindings.some(b=>!['database','schema'].every(k=>typeof b[k]==='string'&&/^[A-Za-z0-9_][A-Za-z0-9_-]{0,127}$/.test(b[k]))))throw Error('PRIVATE_HISTORY_CONFIG');
const store=new PrivateClickHouse({url:required('CLICKHOUSE_URL'),user:required('CLICKHOUSE_USER'),password:required('CLICKHOUSE_PASSWORD'),purpose:'audit'});
const query=new PrivateHistoryQuery({bindings:configuration.bindings,store});
// Query process receives read-only audit credentials and proof verification keys,
// never private encryption keys, delivery credentials or source database DSNs.
const server=httpsServer({key:boundedFile('PRIVATE_HISTORY_TLS_KEY_FILE',65536),cert:boundedFile('PRIVATE_HISTORY_TLS_CERT_FILE',131072),ca:boundedFile('PRIVATE_HISTORY_CLIENT_CA_FILE',1048576),requestCert:true,rejectUnauthorized:true,minVersion:'TLSv1.2'},privateHistoryHandler(query));
const port=Number(process.env.PRIVATE_HISTORY_PORT??8445);if(!Number.isInteger(port)||port<1024||port>65535)throw Error('PRIVATE_HISTORY_PORT');
server.requestTimeout=30000;server.headersTimeout=10000;server.maxRequestsPerSocket=20;server.maxConnections=32;server.listen(port,'0.0.0.0');
for(const signal of ['SIGTERM','SIGINT'])process.once(signal,()=>server.close());
