$ErrorActionPreference = 'Stop'
$artifactRoot = Join-Path (Split-Path (Split-Path $PSScriptRoot -Parent) -Parent) '.loan-implementation'
[IO.Directory]::CreateDirectory($artifactRoot) | Out-Null
$destination = Join-Path $artifactRoot 'payroll-loans.graphql'
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    $schema = & cargo run --quiet --offline -p kabipay-payroll --example export_schema
    if ($LASTEXITCODE -ne 0) { throw 'Payroll SDL export failed' }
    [IO.File]::WriteAllText($destination, ($schema -join "`n"), [Text.UTF8Encoding]::new($false))
} finally { Pop-Location }
