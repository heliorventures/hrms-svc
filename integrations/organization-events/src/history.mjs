import {clickhouseMillis} from './time.mjs';
import {clickhouseQuery} from './clickhouse-http.mjs';
export class ClickHouseHistory {
  constructor({url,user,password,timeout=15000,table='business_history'}) {
    if(!['business_history','reporting_history'].includes(table))throw Error('INVALID_HISTORY_TABLE');
    this.table=table;
    this.url=new URL(url); if(this.url.protocol!=='https:' && process.env.ALLOW_ISOLATED_PLAINTEXT!=='true') throw new Error('CLICKHOUSE_TLS_REQUIRED');
    this.headers={'X-ClickHouse-User':user,'X-ClickHouse-Key':password}; this.timeout=timeout;
  }
  async query(sql,params={},body) {
    return clickhouseQuery({url:this.url,headers:this.headers,timeout:this.timeout,maxBytes:32*1024*1024,errors:{http:status=>`CLICKHOUSE_HTTP_${status}`,size:'CLICKHOUSE_RESPONSE_TOO_LARGE',response:'CLICKHOUSE_RESPONSE',unavailable:'CLICKHOUSE_UNAVAILABLE'}},sql,params,body);
  }
  async write(e) {
    const existing=await this.query(`SELECT event_id,payload_hash,organization_id,source_tenant_id,aggregate_type,aggregate_id,event_type,aggregate_version FROM organization.${this.table} WHERE event_id={event:UUID} OR (organization_id={org:UUID} AND source_tenant_id={tenant:UUID} AND aggregate_type={type:String} AND aggregate_id={aggregate:UUID} AND event_type={kind:String} AND aggregate_version={version:UInt64}) FORMAT JSONEachRow`,{event:e.event_id,org:e.organization_id,tenant:e.source_tenant_id,type:e.aggregate_type,aggregate:e.aggregate_id,kind:e.event_type,version:e.aggregate_version});
    for(const line of existing.trim().split('\n').filter(Boolean)) {
      const row=JSON.parse(line);
      if(row.payload_hash!==e.payload_hash || row.organization_id!==e.organization_id || row.source_tenant_id!==e.source_tenant_id || row.aggregate_type!==e.aggregate_type || row.aggregate_id!==e.aggregate_id || row.event_type!==e.event_type || Number(row.aggregate_version)!==e.aggregate_version) {
        // A permanent historical collision must not overwrite accepted facts.
        const error=new Error('HISTORY_VERSION_CONFLICT'); error.code='HISTORY_VERSION_CONFLICT'; throw error;
      }
    }
    const p=e.payload;
    const {payload_hash,...sourceEvent}=e;
    const row={...e,actor:JSON.stringify(e.actor??null),payload:JSON.stringify(p),occurred_at:clickhouseMillis(e.occurred_at),employee_id:p.employee_id,work_date:p.work_date??p.from_date??'1970-01-01',logged_seconds:p.logged_seconds??0,approved_seconds:p.approved_seconds??0,deleted:Number(e.deleted),project_code:p.project_code??null,observed_seconds:p.observed_seconds??null,overtime_seconds:p.overtime_seconds??null,late_minutes:p.late_minutes??null,early_exit_minutes:p.early_exit_minutes??null,requested_centidays:p.requested_centidays??0,from_date:p.from_date??'1970-01-01',to_date:p.to_date??'1970-01-01',fact_status:p.status??''};
    row.employee_id=p.employee_id??'00000000-0000-0000-0000-000000000000';
    row.source_event=JSON.stringify(sourceEvent);
    await this.query(`INSERT INTO organization.${this.table} SETTINGS async_insert=0 FORMAT JSONEachRow`,{},JSON.stringify(row)+'\n');
  }
}
