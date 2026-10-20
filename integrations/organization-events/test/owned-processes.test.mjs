import test from 'node:test';
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {ownedNodeProcesses} from './owned-processes.mjs';
const exists=pid=>{try{process.kill(pid,0);return true;}catch(error){if(error.code==='ESRCH')return false;throw error;}};

test('timeout cleanup removes its owned child and grandchild process tree',async()=>{
 const owned=ownedNodeProcesses();
 const source="const {spawn}=require('node:child_process');const grand=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore',windowsHide:true});console.log(grand.pid);setInterval(()=>{},1000);";
 const child=owned.spawn(process.execPath,['-e',source],{stdio:['ignore','pipe','pipe'],windowsHide:true});
 let grandchild;
 try{
  grandchild=await new Promise((resolve,reject)=>{let text='';const timer=setTimeout(()=>reject(Error('OWNED_FIXTURE_START_TIMEOUT')),10000);child.stdout.on('data',chunk=>{text+=chunk;if(text.includes('\n')){clearTimeout(timer);resolve(Number(text.trim()));}});child.once('error',error=>{clearTimeout(timer);reject(error);});});
  assert(Number.isSafeInteger(grandchild)&&grandchild>0);assert(exists(child.pid));assert(exists(grandchild));
  await new Promise((resolve,reject)=>setTimeout(()=>owned.terminateAll().then(resolve,reject),100));
  const deadline=Date.now()+5000;while((exists(child.pid)||exists(grandchild))&&Date.now()<deadline)await new Promise(resolve=>setTimeout(resolve,50));
  assert.equal(exists(child.pid),false,'owned child must exit');assert.equal(exists(grandchild),false,'owned grandchild must exit');
 }finally{await owned.terminateAll();}
});

test('acceptance launcher retains opt-in gate before loading any fixture',()=>{
 const env={...process.env};delete env.ORGANIZATION_SYNTHETIC_ACCEPTANCE;
 const result=spawnSync(process.execPath,[fileURLToPath(new URL('./run-acceptance.mjs',import.meta.url)),'isolated-cdc'],{env,encoding:'utf8',timeout:10000,windowsHide:true});
 assert.equal(result.status,1);assert.match(result.stderr,/EXPLICIT_SYNTHETIC_FIXTURE_REQUIRED/);assert.equal(result.stdout,'');
});
