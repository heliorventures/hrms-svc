$ErrorActionPreference = 'Stop'
$serviceRoot = Split-Path $PSScriptRoot -Parent
$destination = Join-Path (Split-Path $serviceRoot -Parent) '.loan-implementation/employee-exit.graphql'
Push-Location $serviceRoot
try {
    $schema = & cargo run --quiet --offline -p kabipay-employee --example export_schema
    if ($LASTEXITCODE -ne 0) { throw 'Employee SDL export failed' }
    [IO.File]::WriteAllText($destination, ($schema -join "`n"), [Text.UTF8Encoding]::new($false))
} finally { Pop-Location }
