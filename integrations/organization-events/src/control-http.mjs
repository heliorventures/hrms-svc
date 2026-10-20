import {clickhouseQuery} from './clickhouse-http.mjs';
// No proxy lookup, redirect following or arbitrary response/error logging.
export class ControlHttp {
 constructor({url,user,password,timeout=15000}){
  this.url=new URL(url);
  if(this.url.username||this.url.password||this.url.search||this.url.hash||this.url.pathname!=='/')throw Error('CONTROL_CLICKHOUSE_URL');
  if(this.url.protocol!=='https:'&&!(process.env.ALLOW_ISOLATED_PLAINTEXT==='true'&&this.url.protocol==='http:'&&['127.0.0.1','localhost','clickhouse'].includes(this.url.hostname)))throw Error('CONTROL_CLICKHOUSE_TLS');
  this.headers={'X-ClickHouse-User':user,'X-ClickHouse-Key':password};this.timeout=timeout;
 }
 async query(sql,params={},body){
  return clickhouseQuery({url:this.url,headers:this.headers,timeout:this.timeout,maxBytes:65536,errors:{http:()=> 'CONTROL_CLICKHOUSE_HTTP',size:'CONTROL_CLICKHOUSE_RESPONSE_SIZE',response:'CONTROL_CLICKHOUSE_RESPONSE',unavailable:'CONTROL_CLICKHOUSE_UNAVAILABLE'}},sql,params,body);
 }
}
