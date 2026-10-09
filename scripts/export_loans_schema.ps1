$ErrorActionPreference = 'Continue'
$artifactRoot = Join-Path (Split-Path (Split-Path $PSScriptRoot -Parent) -Parent) '.loan-implementation'
[IO.Directory]::CreateDirectory($artifactRoot) | Out-Null
$destination = Join-Path $artifactRoot 'payroll-loans.graphql'
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    & rtk proxy cargo run --quiet --offline -p kabipay-payroll --example export_schema | Set-Content -LiteralPath $destination -Encoding UTF8 -ErrorAction Stop
    if ($LASTEXITCODE -ne 0) { throw 'Payroll SDL export failed' }
} finally { Pop-Location }
