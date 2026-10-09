param([switch]$Payroll)
$ErrorActionPreference = 'Stop'
$serviceRoot = Split-Path $PSScriptRoot -Parent
$databaseRoot = Join-Path (Split-Path $serviceRoot -Parent) 'hrms-database'
$containerName = 'hrms-loans-module-' + [guid]::NewGuid().ToString('N')
$fixturePath = Join-Path ([IO.Path]::GetTempPath()) ($containerName + '.sql')
$previousUrl = $env:LOAN_TEST_DATABASE_URL
try {
    $schemaSql = & rtk proxy py -3 (Join-Path $databaseRoot 'tests/migrations/loan_fixture_sql.py')
    if ($LASTEXITCODE -ne 0) { throw 'Loan schema fixture generation failed' }
    $runtimeSql = & rtk proxy py -3 (Join-Path $PSScriptRoot 'loan_runtime_fixture.py')
    if ($LASTEXITCODE -ne 0) { throw 'Upstream fixture generation failed' }
    if ($Payroll) {
        $payrollSql = & rtk proxy py -3 (Join-Path $PSScriptRoot 'payroll_loan_fixture.py')
        if ($LASTEXITCODE -ne 0) { throw 'Payroll fixture generation failed' }
        $runtimeSql += $payrollSql
    }
    [IO.File]::WriteAllText($fixturePath, ($schemaSql -join "`n") + "`n" + ($runtimeSql -join "`n"), [Text.UTF8Encoding]::new($false))
    & rtk proxy docker run --rm -d --name $containerName -p 127.0.0.1::5432 -e POSTGRES_USER=loan_fixture -e POSTGRES_PASSWORD=loan_fixture -e POSTGRES_DB=loan_phase_a postgres:16-alpine
    if ($LASTEXITCODE -ne 0) { throw 'Disposable PostgreSQL startup failed' }
    $ready = $false
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        & rtk proxy docker exec $containerName pg_isready -U loan_fixture -d loan_phase_a | Out-Null
        if ($LASTEXITCODE -eq 0) { $ready = $true; break }
        Start-Sleep -Milliseconds 500
    }
    if (!$ready) { throw 'Disposable PostgreSQL did not become ready' }
    & rtk proxy docker cp $fixturePath "${containerName}:/tmp/loan-fixture.sql"
    if ($LASTEXITCODE -ne 0) { throw 'Fixture copy failed' }
    & rtk proxy docker exec $containerName psql -q -U loan_fixture -d loan_phase_a -f /tmp/loan-fixture.sql
    if ($LASTEXITCODE -ne 0) { throw 'Fixture SQL failed' }
    $binding = (& rtk proxy docker port $containerName 5432/tcp).Trim()
    if ($binding -notmatch '^127\.0\.0\.1:(\d+)$') { throw 'Unexpected test database binding' }
    $env:LOAN_TEST_DATABASE_URL = "postgresql://loan_fixture:loan_fixture@127.0.0.1:$($Matches[1])/loan_phase_a"
    Push-Location $serviceRoot
    try {
        if ($Payroll) { & rtk cargo test -p kabipay-payroll --test loan_recovery --offline -- --ignored --test-threads=1 }
        else { & rtk cargo test -p kabipay-loans --test database --offline -- --ignored --test-threads=1 }
        if ($LASTEXITCODE -ne 0) { throw 'Loan database tests failed' }
    }
    finally { Pop-Location }
} finally {
    $env:LOAN_TEST_DATABASE_URL = $previousUrl
    & rtk proxy docker stop $containerName
    if (Test-Path -LiteralPath $fixturePath) { Remove-Item -LiteralPath $fixturePath }
}
