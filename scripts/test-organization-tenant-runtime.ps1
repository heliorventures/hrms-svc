param(
    [Parameter(Mandatory = $true)][string]$Container,
    [switch]$ResetSyntheticSchema
)
$ErrorActionPreference = 'Stop'
if ($Container -notmatch '^org-pipeline-accept-[a-zA-Z0-9-]+-postgres-1$') {
    throw 'Only the isolated organization acceptance PostgreSQL container is supported.'
}
$workspace = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$fixture = Join-Path $PSScriptRoot 'tests/fixtures/organization_tenant'
$sql = ''
if ($ResetSyntheticSchema) { $sql = "DROP SCHEMA IF EXISTS source_acceptance CASCADE;`n" }
$sql += Get-Content -LiteralPath (Join-Path $fixture 'baseline.sql') -Raw
foreach ($relative in @(
    'hrms-database/changelog/migrations/0112_company_management_foundation/company_management_foundation.xml',
    'hrms-database/changelog/migrations/0115_organization_tenant_runtime/organization_tenant_runtime.xml',
    'hrms-database/changelog/migrations/0117_organization_membership_lifecycle/organization_membership_lifecycle.xml'
)) {
    $document = [xml](Get-Content -LiteralPath (Join-Path $workspace $relative) -Raw)
    foreach ($node in $document.SelectNodes("//*[local-name()='changeSet']/*[local-name()='sql']")) {
        $sql += "`n" + $node.InnerText.Replace('${schema}', 'source_acceptance')
    }
}
$sql += "`n" + (Get-Content -LiteralPath (Join-Path $fixture 'assertions.sql') -Raw)
# Fixed synthetic database, never a deployment URL or tenant selected by input.
$sql | docker exec -i $Container psql -U acceptance -d organization_tenant_acceptance -v ON_ERROR_STOP=1
if ($LASTEXITCODE -ne 0) { throw 'Synthetic source migration/fact assertions failed.' }
Write-Host 'Synthetic PostgreSQL source migration and canonical-fact assertions passed.'
