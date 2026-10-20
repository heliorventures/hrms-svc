import http from 'node:http';
import https from 'node:https';

// Direct sockets intentionally avoid proxy discovery and redirect following.
// Callers own URL policy, schema, response limits and public error vocabulary.
export function clickhouseQuery({url,headers,timeout,maxBytes,errors},sql,params={},body) {
 const target=new URL(url);target.searchParams.set('query',sql);
 for(const [key,value] of Object.entries(params))target.searchParams.set(`param_${key}`,String(value));
 return new Promise((resolve,reject)=>{
  const request=(target.protocol==='https:'?https:http).request(target,{method:'POST',headers},response=>{
   if(response.statusCode!==200){response.resume();reject(Error(errors.http(response.statusCode)));return;}
   const chunks=[];let bytes=0;
   response.on('data',chunk=>{bytes+=chunk.length;if(bytes>maxBytes){response.destroy();reject(Error(errors.size));}else chunks.push(chunk);});
   response.on('end',()=>resolve(Buffer.concat(chunks).toString('utf8')));
   response.on('error',()=>reject(Error(errors.response)));
  });
  // A total deadline also bounds slow/trickling responses, unlike socket idle timeout.
  const deadline=setTimeout(()=>request.destroy(),timeout);
  request.on('close',()=>clearTimeout(deadline));
  request.on('error',()=>reject(Error(errors.unavailable)));
  request.end(body);
 });
}
