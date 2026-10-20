import {clickhouseMillis} from './time.mjs';
export class ControlHistory {
 constructor(client){this.client=client;}
 async write(event){
  const rows=await this.client.query('SELECT event_id,content_hash FROM organization.control_history FINAL WHERE event_id={event:UUID} OR (metadata_source={source:String} AND aggregate_id={aggregate:UUID} AND aggregate_version={version:UInt64}) LIMIT 2 FORMAT JSONEachRow',{event:event.event_id,source:event.metadata_source,aggregate:event.aggregate_id,version:event.aggregate_version});
  for(const line of rows.trim().split('\n').filter(Boolean)){
   const row=JSON.parse(line);if(row.event_id!==event.event_id||row.content_hash!==event.content_hash){const error=new Error('CONTROL_HISTORY_CONFLICT');error.code='CONTROL_HISTORY_CONFLICT';throw error;}
  }
  await this.client.query('INSERT INTO organization.control_history SETTINGS async_insert=0 FORMAT JSONEachRow',{},JSON.stringify({...event,occurred_at:clickhouseMillis(event.occurred_at)})+'\n');
 }
 async checkpoint(position){
  const current=await this.client.query('SELECT state,first_quarantine_offset FROM organization.control_capture_checkpoint FINAL WHERE topic={topic:String} AND partition={partition:UInt32} FORMAT JSONEachRow',position);
  const previous=current.trim()?JSON.parse(current.trim().split('\n')[0]):null;
  const partial=position.state==='QUARANTINED'||previous?.state==='QUARANTINED';
  const row={...position,state:partial?'QUARANTINED':'READY',first_quarantine_offset:previous?.first_quarantine_offset??(partial?position.offset:null)};
  await this.client.query('INSERT INTO organization.control_capture_checkpoint SETTINGS async_insert=0 FORMAT JSONEachRow',{},JSON.stringify(row)+'\n');
 }
}
