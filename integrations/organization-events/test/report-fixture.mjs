// Called only AFTER application policy resolves exact organization/company scope.
// Date filtering follows latest-version selection in the database view.
export async function progress(history,{organizationId,tenantIds,from,to}) {
  if(!Array.isArray(tenantIds)||tenantIds.length===0) throw Error('COMPANY_SCOPE_REQUIRED');
  const rows=[];
  for(const tenant of [...new Set(tenantIds)]) {
    const result=await history.query(`SELECT source_tenant_id,work_date,toString(sum(logged_seconds)) AS logged_seconds,toString(sum(approved_seconds)) AS approved_seconds FROM organization.progress_daily WHERE organization_id={organization:UUID} AND source_tenant_id={tenant:UUID} AND work_date>={from:Date} AND work_date<={to:Date} GROUP BY source_tenant_id,work_date ORDER BY work_date FORMAT JSONEachRow`,{organization:organizationId,tenant,from,to});
    rows.push(...result.trim().split('\n').filter(Boolean).map(line=>JSON.parse(line)));
  }
  // Missing rows are deliberately not filled with zero. Application must merge
  // expected partitions/capture acceptance and metadata watermarks independently.
  return {rows,requestedTenantIds:[...new Set(tenantIds)],coverageRequired:true};
}
