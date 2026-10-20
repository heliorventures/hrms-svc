// Local synthetic acceptance only. Captures exact owned process handles so a
// stalled fixture can stop without guessing Node PIDs or touching Docker state.
import childProcess from 'node:child_process';
import {syncBuiltinESMExports} from 'node:module';
import {ownedNodeProcesses} from './owned-processes.mjs';
const fixtures={
 'isolated-acceptance':180000,
 'isolated-cdc':180000,
 'control-cdc-acceptance':240000,
 'historical-retention-acceptance':180000,
 'control-cleanup-acceptance':180000,
 'gap-lifecycle-acceptance':900000,
};
const name=process.argv[2];
if(process.env.ORGANIZATION_SYNTHETIC_ACCEPTANCE!=='true'||!Object.hasOwn(fixtures,name)||process.argv.length!==3)throw Error('EXPLICIT_SYNTHETIC_FIXTURE_REQUIRED');
const spawn=childProcess.spawn;
const owned=ownedNodeProcesses({onSpawn:pid=>console.log(JSON.stringify({fixture:name,owned_child_pid:pid}))});
childProcess.spawn=(...args)=>owned.spawn(...args);
syncBuiltinESMExports();
console.log(JSON.stringify({fixture:name,pid:process.pid,started_at:new Date().toISOString(),timeout_ms:fixtures[name]}));
let stopping=false;
async function stopOwned(code,reason){
 if(stopping)return;stopping=true;
 console.error(JSON.stringify({fixture:name,result:reason,owned_child_pids:owned.pids(),synthetic_resources_preserved_for_reconciliation:true}));
 try{await owned.terminateAll();}catch{console.error(JSON.stringify({fixture:name,result:'OWNED_PROCESS_TREE_CLEANUP_FAILED'}));code=1;}
 // Deliberately no Docker, SQL, connector deletion or unrelated PID searches.
 process.exit(code);
}
process.once('SIGINT',()=>stopOwned(130,'INTERRUPTED'));
process.once('SIGTERM',()=>stopOwned(143,'INTERRUPTED'));
const watchdog=setTimeout(()=>stopOwned(124,'TIMEOUT'),fixtures[name]);
try{
 await import(new URL(`./${name}.mjs`,import.meta.url));
 await owned.terminateAll();
 console.log(JSON.stringify({fixture:name,result:'COMPLETED',owned_child_pids:owned.pids()}));
}catch(error){
 console.error(JSON.stringify({fixture:name,error_code:error?.code??error?.name,message:String(error?.message??'fixture failed').slice(0,500),causes:Array.from(error?.errors??[]).slice(0,4).map(cause=>({error_code:cause?.code??cause?.name,message:String(cause?.message??'fixture failed').slice(0,300)}))}));
 await stopOwned(1,'FAILED');
}finally{
 clearTimeout(watchdog);
 childProcess.spawn=spawn;syncBuiltinESMExports();
}
