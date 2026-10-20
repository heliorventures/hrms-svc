export class ProjectionStore {
  constructor(pool) { this.pool=pool; }
  async transaction(fn) {
    const client=await this.pool.connect();
    try { await client.query('BEGIN'); const result=await fn(new ProjectionTransaction(client)); await client.query('COMMIT'); return result; }
    catch(error) { await client.query('ROLLBACK'); throw error; }
    finally { client.release(); }
  }
  async markConsumerPartial({topic,partition,offset,binding,code,role}) {
    if(!binding)throw Error('UNTRUSTED_TOPIC');
    await this.transaction(async tx=>{for(const organization of binding.organization_ids??[binding.organization_id])await tx.checkpoint({organization_id:organization,source_tenant_id:binding.tenant_id,occurred_at:null},{topic,partition,offset,code,role,state:'PARTIAL'});});
  }
  async barrier(e,m) {
    return this.transaction(async tx=>{
      const classification=(await tx.client.query('SELECT kabipay_ops.organization_capture_marker_classification($1,$2,$3,$4) AS state',[e.organization_id,e.source_tenant_id,e.membership_id,e.membership_version])).rows[0]?.state;
      if(!['ACTIVE','HISTORICAL','NONE'].includes(classification))throw Error('MEMBERSHIP_CLASSIFICATION_UNAVAILABLE');
      const marker=(await tx.client.query(`SELECT marker_id FROM kabipay_ops.organization_capture_marker WHERE marker_id=$1 AND organization_id=$2 AND source_tenant_id=$3 AND membership_id=$4 AND membership_version=$5 AND topic=$6 AND $7=ANY(expected_partitions)`,[e.marker_id,e.organization_id,e.source_tenant_id,e.membership_id,e.membership_version,m.topic,m.partition])).rows[0];
      if(!marker)throw Error('UNCONFIRMED_CAPTURE_BARRIER');
      // Already-published barriers can also arrive after suspension or resume.
      // Confirm immutable provenance first, then acknowledge without READY proof.
      if(classification==='HISTORICAL')return {historical:true};
      if(classification==='NONE')throw Error('CAPTURE_MEMBERSHIP_UNRECOGNIZED');
      await tx.client.query(`INSERT INTO kabipay_ops.organization_capture_barrier(marker_id,consumer_role,partition_id,kafka_offset) VALUES($1,$2,$3,$4) ON CONFLICT(marker_id,consumer_role,partition_id) DO UPDATE SET kafka_offset=GREATEST(organization_capture_barrier.kafka_offset,EXCLUDED.kafka_offset),acknowledged_at=clock_timestamp()`,[e.marker_id,m.role,m.partition,m.offset]);
      await tx.checkpoint({...e,occurred_at:null},{...m,state:'READY'});
    });
  }
}
class ProjectionTransaction {
  constructor(client) { this.client=client; }
  async identity(e,role) {
    // Retain hash fences for ALL accepted versions. A later version must not
    // erase the evidence needed to detect a conflicting historical replay.
    const key=[role,e.organization_id,e.source_tenant_id,e.aggregate_type,e.aggregate_id,e.event_type,e.aggregate_version].join(':');
    await this.client.query('SELECT pg_advisory_xact_lock(hashtextextended($1,0))',[`identity:${role}:${e.event_id}`]);
    await this.client.query('SELECT pg_advisory_xact_lock(hashtextextended($1,0))',[key]);
    const rows=(await this.client.query(`SELECT event_id,organization_id,source_tenant_id,aggregate_type,aggregate_id,event_type,source_version,payload_hash FROM kabipay_ops.organization_consumer_identity WHERE consumer_role=$1 AND (event_id=$2 OR (organization_id=$3 AND source_tenant_id=$4 AND aggregate_type=$5 AND aggregate_id=$6 AND event_type=$7 AND source_version=$8))`,[role,e.event_id,e.organization_id,e.source_tenant_id,e.aggregate_type,e.aggregate_id,e.event_type,e.aggregate_version])).rows;
    if(rows.some(r=>r.payload_hash!==e.payload_hash||r.organization_id!==e.organization_id||r.source_tenant_id!==e.source_tenant_id||r.aggregate_type!==e.aggregate_type||r.aggregate_id!==e.aggregate_id||r.event_type!==e.event_type||Number(r.source_version)!==e.aggregate_version))return false;
    await this.client.query(`INSERT INTO kabipay_ops.organization_consumer_identity(consumer_role,event_id,organization_id,source_tenant_id,aggregate_type,aggregate_id,event_type,source_version,payload_hash) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT DO NOTHING`,[role,e.event_id,e.organization_id,e.source_tenant_id,e.aggregate_type,e.aggregate_id,e.event_type,e.aggregate_version,e.payload_hash]);
    return true;
  }
  async checkpoint(e,m) {
    await this.client.query(`INSERT INTO kabipay_ops.organization_consumer_checkpoint(consumer_role,organization_id,source_tenant_id,topic,partition_id,kafka_offset,source_observed_at,state,detail_code) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(consumer_role,organization_id,source_tenant_id,topic,partition_id) DO UPDATE SET kafka_offset=GREATEST(organization_consumer_checkpoint.kafka_offset,EXCLUDED.kafka_offset),source_observed_at=GREATEST(organization_consumer_checkpoint.source_observed_at,EXCLUDED.source_observed_at),acknowledged_at=clock_timestamp(),state=CASE WHEN organization_consumer_checkpoint.state='QUARANTINED' THEN organization_consumer_checkpoint.state WHEN EXCLUDED.state IN ('PARTIAL','QUARANTINED') THEN EXCLUDED.state WHEN organization_consumer_checkpoint.state='PARTIAL' THEN organization_consumer_checkpoint.state ELSE EXCLUDED.state END,detail_code=CASE WHEN organization_consumer_checkpoint.state='QUARANTINED' THEN organization_consumer_checkpoint.detail_code WHEN EXCLUDED.state IN ('PARTIAL','QUARANTINED') THEN EXCLUDED.detail_code WHEN organization_consumer_checkpoint.state='PARTIAL' THEN organization_consumer_checkpoint.detail_code ELSE EXCLUDED.detail_code END`,[m.role,e.organization_id,e.source_tenant_id,m.topic,m.partition,m.offset,e.occurred_at,m.state,m.code??null]);
  }
  async membershipClassification(e) {
    const result=await this.client.query('SELECT kabipay_ops.organization_capture_member_classification($1,$2) AS classification',[e.organization_id,e.source_tenant_id]);
    const state=result.rows[0]?.classification;
    if(!['ACTIVE','HISTORICAL','NONE'].includes(state))throw Error('MEMBERSHIP_CLASSIFICATION_UNAVAILABLE');
    return state;
  }
  async lock(e) {
    const key=[e.organization_id,e.source_tenant_id,e.aggregate_type,e.aggregate_id,e.event_type];
    await this.client.query('SELECT pg_advisory_xact_lock(hashtextextended($1,0))',['event:'+e.event_id]);
    // Transaction advisory lock also serializes first insertion, including replays.
    await this.client.query('SELECT pg_advisory_xact_lock(hashtextextended($1,0))',[key.join(':')]);
    return (await this.client.query('SELECT source_version,payload_hash FROM kabipay_ops.organization_projection_fence WHERE organization_id=$1 AND source_tenant_id=$2 AND aggregate_type=$3 AND aggregate_id=$4 AND event_type=$5 FOR UPDATE',key)).rows[0];
  }
  async apply(e) {
    const p=e.payload;
    if(e.event_type==='approval.changed') await this.client.query(`INSERT INTO kabipay_ops.organization_inbox_projection(organization_id,source_tenant_id,resource_type,resource_id,employee_id,employee_display_name,status,summary,source_version,deleted,occurred_at,payload_hash)
      VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) ON CONFLICT(organization_id,source_tenant_id,resource_type,resource_id) DO UPDATE SET employee_id=EXCLUDED.employee_id,employee_display_name=EXCLUDED.employee_display_name,status=EXCLUDED.status,summary=EXCLUDED.summary,source_version=EXCLUDED.source_version,deleted=EXCLUDED.deleted,occurred_at=EXCLUDED.occurred_at,payload_hash=EXCLUDED.payload_hash,projected_at=now()`,[e.organization_id,e.source_tenant_id,p.resource_type,p.resource_id,p.employee_id,p.employee_display_name,p.status,p.summary,e.aggregate_version,e.deleted,e.occurred_at,e.payload_hash]);
    if(e.event_type==='directory.changed') await this.client.query(`INSERT INTO kabipay_ops.organization_directory_projection(organization_id,source_tenant_id,employee_id,display_name,employment_status,source_version,deleted,occurred_at,payload_hash) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(organization_id,source_tenant_id,employee_id) DO UPDATE SET display_name=EXCLUDED.display_name,employment_status=EXCLUDED.employment_status,source_version=EXCLUDED.source_version,deleted=EXCLUDED.deleted,occurred_at=EXCLUDED.occurred_at,payload_hash=EXCLUDED.payload_hash,projected_at=now()`,[e.organization_id,e.source_tenant_id,p.employee_id,p.display_name,p.employment_status,e.aggregate_version,e.deleted,e.occurred_at,e.payload_hash]);
    await this.client.query(`INSERT INTO kabipay_ops.organization_projection_fence VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(organization_id,source_tenant_id,aggregate_type,aggregate_id,event_type) DO UPDATE SET source_version=EXCLUDED.source_version,payload_hash=EXCLUDED.payload_hash`,[e.organization_id,e.source_tenant_id,e.aggregate_type,e.aggregate_id,e.event_type,e.aggregate_version,e.payload_hash]);
  }
  async watermark(e,m) {
    await this.client.query(`INSERT INTO kabipay_ops.organization_projection_watermark(organization_id,source_tenant_id,topic,partition_id,kafka_offset,source_observed_at,state,detail_code) VALUES($1,$2,$3,$4,$5,$6,$7,$8)
     ON CONFLICT(organization_id,source_tenant_id,topic,partition_id) DO UPDATE SET kafka_offset=GREATEST(organization_projection_watermark.kafka_offset,EXCLUDED.kafka_offset),source_observed_at=GREATEST(organization_projection_watermark.source_observed_at,EXCLUDED.source_observed_at),projected_at=now(),state=CASE WHEN organization_projection_watermark.state IN ('PARTIAL','QUARANTINED') THEN organization_projection_watermark.state ELSE EXCLUDED.state END,detail_code=COALESCE(organization_projection_watermark.detail_code,EXCLUDED.detail_code)`,[e.organization_id,e.source_tenant_id,m.topic,m.partition,m.offset,e.occurred_at,m.state,m.code??null]);
  }
}
