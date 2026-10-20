// Node22 source context preserves an original integer token before Number can
// round a large PostgreSQL LSN. Other event quantities retain strict safe-int
// validation; only fixed CDC offset fields are decoded as decimal strings.
export function parseCdcJson(input){return JSON.parse(input,(key,value,context)=>{
 if(['lsn','lsn_commit','lsn_proc'].includes(key)&&typeof value==='number'){
  if(!context||!/^\d+$/.test(context.source))throw Error('CDC_OFFSET_INTEGER_REQUIRED');return context.source;
 }
 return value;
});}
