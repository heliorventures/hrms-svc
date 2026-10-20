import { createHash } from 'node:crypto';
import {ADMINISTRATION_ACTIONS} from './operations.mjs';
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const envelope = ['schema_version','event_id','event_type','organization_id','source_tenant_id','aggregate_type','aggregate_id','aggregate_version','occurred_at','deleted','payload'];
export class InvalidEvent extends Error { constructor(code) { super(code); this.code=code; } }
function requireValue(condition, code) { if (!condition) throw new InvalidEvent(code); }
function exact(object, keys) { requireValue(object && typeof object==='object' && !Array.isArray(object), 'OBJECT_REQUIRED'); requireValue(Object.keys(object).every(k=>keys.includes(k)) && keys.every(k=>Object.hasOwn(object,k)), 'FIELD_SET'); }
function date(value){return typeof value==='string'&&/^\d{4}-\d\d-\d\d$/.test(value)&&Number.isFinite(Date.parse(value))&&new Date(value).toISOString().slice(0,10)===value;}
function quantity(value,nullable=false){return (nullable&&value===null)||(Number.isSafeInteger(value)&&value>=0);}
export function stable(value) { return JSON.stringify(value && typeof value==='object' ? Array.isArray(value) ? value.map(v=>JSON.parse(stable(v))) : Object.fromEntries(Object.keys(value).sort().map(k=>[k,JSON.parse(stable(value[k]))])) : value); }
export function digest(value) { return createHash('sha256').update(typeof value==='string'||Buffer.isBuffer(value)?value:stable(value)).digest('hex'); }
export function parseEvent(raw, topic, bindings) {
  requireValue(Buffer.isBuffer(raw)&&raw.length<=131072,'EVENT_TOO_LARGE');
  let e;try {e=JSON.parse(raw.toString());if(typeof e==='string'){requireValue(Buffer.byteLength(e)<=65536,'EVENT_TOO_LARGE');e=JSON.parse(e);}else requireValue(raw.length<=65536,'EVENT_TOO_LARGE');}catch(error){if(error instanceof InvalidEvent)throw error;throw new InvalidEvent('INVALID_JSON');}
  exact(e,Object.hasOwn(e??{},'actor')?[...envelope,'actor']:envelope);
  requireValue(e.schema_version===1,'SCHEMA_VERSION');
  for (const k of ['event_id','organization_id','source_tenant_id','aggregate_id']) requireValue(typeof e[k]==='string'&&UUID.test(e[k]),'UUID');
  const binding=bindings[topic];
  const organizations=binding?.organization_ids??[binding?.organization_id];
  requireValue(binding && binding.tenant_id===e.source_tenant_id && Array.isArray(organizations)&&organizations.includes(e.organization_id),'TOPIC_PROVENANCE');
  requireValue(Number.isSafeInteger(e.aggregate_version)&&e.aggregate_version>0,'VERSION');
  requireValue(typeof e.deleted==='boolean' && typeof e.occurred_at==='string' && /^\d{4}-\d\d-\d\dT/.test(e.occurred_at)&&Number.isFinite(Date.parse(e.occurred_at)),'ENVELOPE');
  const actor=e.actor;
  if(actor!==undefined&&actor!==null) {
    if(actor.kind==='organization_principal') {
      exact(actor,['kind','principal_id','display_name','identity_version','source_tenant_id','source_user_id','authority_kind','authority_id','authority_version','command_id']);
      for(const key of ['principal_id','source_tenant_id','source_user_id','authority_id','command_id'])requireValue(typeof actor[key]==='string'&&UUID.test(actor[key]),'ACTOR_UUID');
      requireValue(typeof actor.display_name==='string'&&[...actor.display_name].length<=255,'ACTOR_NAME');
      requireValue(['organization_grant','management_assignment'].includes(actor.authority_kind),'ACTOR_AUTHORITY');
      for(const key of ['identity_version','authority_version'])requireValue(Number.isSafeInteger(actor[key])&&actor[key]>0,'ACTOR_VERSION');
    } else if(actor.kind==='local_user') {
      exact(actor,['kind','local_user_id','source_tenant_id']);
      requireValue(typeof actor.local_user_id==='string'&&UUID.test(actor.local_user_id)&&actor.source_tenant_id===e.source_tenant_id,'ACTOR_LOCAL_PROVENANCE');
    } else if(actor.kind==='system') exact(actor,['kind']);
    else throw new InvalidEvent('ACTOR_KIND');
  }
  const p=e.payload;
  if(e.event_type==='approval.changed') {
    exact(p,['resource_type','resource_id','employee_id','employee_display_name','status','summary']);
    requireValue(['leave_request','performance_participant','timesheet'].includes(p.resource_type)&&p.resource_type===e.aggregate_type&&p.resource_id===e.aggregate_id,'RESOURCE');
    requireValue(typeof p.employee_id==='string'&&UUID.test(p.employee_id),'EMPLOYEE');
    for(const k of ['employee_display_name','status','summary']) requireValue(typeof p[k]==='string'&&[...p[k]].length<=(k==='summary'?240:k==='employee_display_name'?255:120),'TEXT');
    requireValue(/^[A-Z][A-Z0-9_]*$/.test(p.status),'CANONICAL_STATUS');
  } else if(e.event_type==='timesheet.changed') {
    exact(p,['employee_id','work_date','logged_seconds','approved_seconds']);
    requireValue(e.aggregate_type==='timesheet_entry'&&typeof p.employee_id==='string'&&UUID.test(p.employee_id),'RESOURCE');
    requireValue(typeof p.work_date==='string'&&/^\d{4}-\d\d-\d\d$/.test(p.work_date)&&Number.isFinite(Date.parse(p.work_date))&&new Date(p.work_date).toISOString().slice(0,10)===p.work_date,'DATE');
    for(const k of ['logged_seconds','approved_seconds']) requireValue(Number.isSafeInteger(p[k])&&p[k]>=0,'SECONDS');
  } else if(e.event_type==='administration.changed') {
    exact(p,['resource_type','resource_id','action','status']);
    requireValue(p.resource_type===e.aggregate_type&&p.resource_id===e.aggregate_id&&ADMINISTRATION_ACTIONS[p.resource_type]?.includes(p.action),'ADMINISTRATION_ACTION');
    requireValue(p.status===null||['RECORDED','QUEUED','RUNNING','SUCCEEDED','FAILED','CANCELLED'].includes(p.status),'ADMINISTRATION_STATUS');
  } else if(e.event_type==='legacy_audit.recorded') {
    exact(p,['source_audit_id','source_entity_type','source_entity_id','source_action','source_user_id','before_status','after_status','source_sha256']);
    requireValue(e.aggregate_type==='legacy_audit'&&p.source_audit_id===e.aggregate_id&&e.aggregate_version===1&&!e.deleted,'RESOURCE');
    requireValue(['LEAVE_REQUEST','employee_profile_change_request','employee_education','employee_work_experience','company_location','working_calendar_profile','holiday','roster','UNMAPPED'].includes(p.source_entity_type),'LEGACY_ENTITY');
    requireValue(['approved','rejected','submitted','created','updated','deleted','APPROVED','REJECTED','CREATE','UPDATE','DELETE','CANCEL_APPROVED_COMP_OFF_LEAVE','UNMAPPED'].includes(p.source_action),'LEGACY_ACTION');
    for(const k of ['source_entity_id','source_user_id'])requireValue(p[k]===null||(typeof p[k]==='string'&&UUID.test(p[k])),'UUID');
    for(const k of ['before_status','after_status'])requireValue(p[k]===null||['PENDING','APPROVED','REJECTED','CANCELLED','VERIFIED','DRAFT','SUBMITTED','ACTIVE','INACTIVE'].includes(p[k]),'LEGACY_STATUS');
    requireValue(typeof p.source_sha256==='string'&&/^[a-f0-9]{64}$/.test(p.source_sha256),'LEGACY_HASH');
  } else if(e.event_type==='attendance.changed') {
    exact(p,['employee_id','work_date','status','observed_seconds','overtime_seconds','late_minutes','early_exit_minutes']);
    requireValue(e.aggregate_type==='attendance'&&typeof p.employee_id==='string'&&UUID.test(p.employee_id)&&date(p.work_date),'RESOURCE');
    requireValue(typeof p.status==='string'&&/^[A-Z][A-Z0-9_]{0,39}$/.test(p.status),'CANONICAL_STATUS');
    for(const k of ['observed_seconds','overtime_seconds','late_minutes','early_exit_minutes'])requireValue(quantity(p[k],true),'QUANTITY');
  } else if(e.event_type==='leave.changed') {
    exact(p,['employee_id','from_date','to_date','requested_centidays','status']);
    requireValue(e.aggregate_type==='leave_request'&&typeof p.employee_id==='string'&&UUID.test(p.employee_id)&&date(p.from_date)&&date(p.to_date)&&p.to_date>=p.from_date,'RESOURCE');
    requireValue(quantity(p.requested_centidays)&&typeof p.status==='string'&&/^[A-Z][A-Z0-9_]{0,39}$/.test(p.status),'QUANTITY');
  } else if(e.event_type==='project_time.changed') {
    exact(p,['employee_id','work_date','project_code','logged_seconds','approved_seconds']);
    requireValue(e.aggregate_type==='timesheet_entry'&&typeof p.employee_id==='string'&&UUID.test(p.employee_id)&&date(p.work_date),'RESOURCE');
    requireValue(p.project_code===null||(typeof p.project_code==='string'&&[...p.project_code].length<=255),'PROJECT_CODE');
    requireValue(quantity(p.logged_seconds)&&quantity(p.approved_seconds),'QUANTITY');
  } else if(e.event_type==='directory.changed') {
    exact(p,['employee_id','display_name','employment_status']);
    requireValue(e.aggregate_type==='employee'&&p.employee_id===e.aggregate_id,'RESOURCE');
    requireValue(typeof p.display_name==='string'&&[...p.display_name].length<=255&&typeof p.employment_status==='string'&&p.employment_status.length<=40,'TEXT');
  } else throw new InvalidEvent('EVENT_TYPE');
  return {...e,payload_hash:digest({event_type:e.event_type,occurred_at:e.occurred_at,deleted:e.deleted,payload:p,...(actor?{actor}:{})})};
}

