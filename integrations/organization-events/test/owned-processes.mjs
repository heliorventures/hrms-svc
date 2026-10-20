import childProcess from 'node:child_process';

// Only handles returned by this manager's own Node spawns are eligible. Windows
// terminates their trees before the launcher exits; POSIX children get a private
// process group so their descendants can be terminated as one owned group.
export function ownedNodeProcesses({onSpawn=()=>{}}={}) {
 const children=new Set(),spawn=childProcess.spawn;
 function active(){return [...children].filter(child=>child.exitCode===null&&child.signalCode===null);}
 return {
  spawn(file,args=[],options={}) {
   if(!Array.isArray(args)){options=args;args=[];}
   const owned=file===process.execPath;
   const child=spawn(file,args,owned&&process.platform!=='win32'?{...options,detached:true}:options);
   if(owned){child.once('spawn',()=>{children.add(child);onSpawn(child.pid);});child.once('exit',()=>children.delete(child));}
   return child;
  },
  pids:()=>active().map(child=>child.pid),
  async terminateAll(){
   for(const child of active()){
    const exited=new Promise(resolve=>child.once('exit',resolve));
    if(process.platform==='win32'){
     const result=childProcess.spawnSync('taskkill.exe',['/PID',String(child.pid),'/T','/F'],{shell:false,windowsHide:true,timeout:10000,stdio:'pipe'});
     // A naturally exiting child can disappear while taskkill starts. Its exact
     // handle must still report exit before cleanup can be claimed successful.
     if(result.error&&child.exitCode===null&&child.signalCode===null)throw Error('OWNED_PROCESS_TREE_TERMINATION_FAILED');
    }else{
     try{process.kill(-child.pid,'SIGKILL');}catch(error){if(error.code!=='ESRCH')throw error;}
    }
    let timer;
    try{await Promise.race([exited,new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('OWNED_PROCESS_TREE_EXIT_TIMEOUT')),10000);})]);}
    finally{clearTimeout(timer);}
   }
  },
 };
}
