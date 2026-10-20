import {parseControlEvent} from './control-events.mjs';
import {timestampNanos} from './time.mjs';
import {digest} from './events.mjs';
function timestampForms(value){const n=timestampNanos(value),seconds=n/1000000000n,fraction=(n%1000000000n).toString().padStart(9,'0').replace(/0+$/,'');const prefix=new Date(Number(seconds*1000n)).toISOString().slice(0,19),forms=new Set([value]);for(const width of [0,1,2,3,4,5,6,7,8,9])if(fraction.length<=width){const time=prefix+(width?'.'+fraction.padEnd(width,'0'):'');forms.add(time+'Z');forms.add(time+'+00:00');}return [...forms];}
// Operator-only verifier. No global NULL-org security history is exposed to the
// organization app; this uses a separately configured control archive identity.
export function controlRetentionVerifier({history,binding}){
 return async({rows,cutover})=>{
  const checkpoint=JSON.parse((await history.query('SELECT state,toString(offset) AS kafka_offset FROM organization.control_capture_checkpoint FINAL WHERE topic={topic:String} AND partition=0 FORMAT JSONEachRow',{topic:binding.topic})).trim()||'null');
  const final=cutover.legacy_final_offsets.find(t=>t.topic===binding.topic)?.partitions.find(p=>p.partition===0)?.offset;
  if(!checkpoint||checkpoint.state!=='READY'||!/^\d+$/.test(checkpoint.kafka_offset)||!/^\d+$/.test(final??'')||BigInt(checkpoint.kafka_offset)+1n<BigInt(final))return false;
  for(const row of rows){
   const event=parseControlEvent(Buffer.from(JSON.stringify({op:'c',source:{db:binding.database,schema:binding.schema,table:'organization_outbox',name:binding.connector_name},after:row.source_record})),binding.topic,binding);
   const text=await history.query('SELECT event_id,content_hash FROM organization.control_history FINAL WHERE metadata_source={source:String} AND event_id={id:UUID} FORMAT JSONEachRow',{source:binding.connector_name,id:event.event_id});
   // PostgreSQL JSONB and Debezium can render the SAME precise timestamp with
   // different UTC suffix/fraction widths. Try only equivalent instants; every
   // original event field still participates in the retained exact SHA256.
   const {content_hash,...fields}=event,hashes=new Set(timestampForms(event.occurred_at).map(occurred_at=>digest({...fields,occurred_at})));
   const retained=text.trim().split('\n').filter(Boolean).map(JSON.parse);if(retained.length!==1||retained[0].event_id!==event.event_id||!hashes.has(retained[0].content_hash))return false;
  }
  return true;
 };
}
