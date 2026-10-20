import test from 'node:test';
import assert from 'node:assert/strict';
import {ClickHouseHistory} from '../src/history.mjs';
import {progress} from './report-fixture.mjs';
import http from 'node:http';
import {ControlHttp} from '../src/control-http.mjs';
test('HTTP query binds values as URL parameters and never returns raw server errors',async()=>{
 const previous=process.env.ALLOW_ISOLATED_PLAINTEXT;process.env.ALLOW_ISOLATED_PLAINTEXT='true';let seen;
 const server=http.createServer((request,response)=>{seen=new URL(request.url,'http://localhost');response.writeHead(400);response.end('sensitive raw error');});
 await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
 try{for(const Client of [ClickHouseHistory,ControlHttp]){const history=new Client({url:`http://127.0.0.1:${server.address().port}`,user:'worker',password:'secret'});await assert.rejects(history.query('SELECT {id:String}',{id:"x';DROP TABLE a;--"}),e=>e.message===(Client===ControlHttp?'CONTROL_CLICKHOUSE_HTTP':'CLICKHOUSE_HTTP_400'));assert.equal(seen.searchParams.get('param_id'),"x';DROP TABLE a;--");assert.equal(seen.searchParams.get('query'),'SELECT {id:String}');}}finally{server.closeAllConnections();await new Promise(resolve=>server.close(resolve));if(previous===undefined)delete process.env.ALLOW_ISOLATED_PLAINTEXT;else process.env.ALLOW_ISOLATED_PLAINTEXT=previous;}
});

test('shared transport bounds slow responses and keeps control response limit separate',async()=>{
 const previous=process.env.ALLOW_ISOLATED_PLAINTEXT;process.env.ALLOW_ISOLATED_PLAINTEXT='true';
 const server=http.createServer((request,response)=>{if(request.url.includes('slow')){response.writeHead(200);response.write('x');}else response.end('x'.repeat(65537));});
 await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
 try{const config={url:`http://127.0.0.1:${server.address().port}`,user:'worker',password:'secret',timeout:50};await assert.rejects(new ControlHttp(config).query('large'),/CONTROL_CLICKHOUSE_RESPONSE_SIZE/);assert.equal((await new ClickHouseHistory(config).query('large')).length,65537);await assert.rejects(new ControlHttp(config).query('slow'),/CONTROL_CLICKHOUSE_(UNAVAILABLE|RESPONSE)/);}finally{server.closeAllConnections();await new Promise(resolve=>server.close(resolve));if(previous===undefined)delete process.env.ALLOW_ISOLATED_PLAINTEXT;else process.env.ALLOW_ISOLATED_PLAINTEXT=previous;}
});
test('reports require explicit companies, preserve UInt64 strings and flag coverage',async()=>{
 let calls=0;const history={query:async(sql,params)=>{calls++;assert(sql.includes('{organization:UUID}'));assert.equal(params.tenant,'company');return '{"logged_seconds":"9007199254740993","approved_seconds":"0"}\n';}};
 await assert.rejects(progress(history,{organizationId:'org',tenantIds:[],from:'2026-01-01',to:'2026-01-02'}));
 const result=await progress(history,{organizationId:'org',tenantIds:['company','company'],from:'2026-01-01',to:'2026-01-02'});assert.equal(result.rows[0].logged_seconds,'9007199254740993');assert.equal(result.coverageRequired,true);assert.equal(calls,1);
});
