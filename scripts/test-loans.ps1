param([switch]$Payroll, [switch]$Exit)
$ErrorActionPreference = 'Stop'
$serviceRoot = Split-Path $PSScriptRoot -Parent
$databaseRoot = Join-Path (Split-Path $serviceRoot -Parent) 'hrms-database'
$testTempRoot = Join-Path (Split-Path $serviceRoot -Parent) '.loan-implementation/tmp'
[IO.Directory]::CreateDirectory($testTempRoot) | Out-Null
$previousTemp = $env:TEMP
$previousTmp = $env:TMP
$env:TEMP = $testTempRoot
$env:TMP = $testTempRoot
$containerName = 'hrms-loans-module-' + [guid]::NewGuid().ToString('N')
$fixturePath = Join-Path ([IO.Path]::GetTempPath()) ($containerName + '.sql')
$previousUrl = $env:LOAN_TEST_DATABASE_URL
$created = $false
try {
    # Compile before starting the disposable database. A long link must not consume
    # fixture lifetime; compilation failure must not leave a test container running.
    Push-Location $serviceRoot
    try {
        if ($Exit) { & cargo test -p kabipay-employee --test exit_finance --offline --no-run }
        elseif ($Payroll) { & cargo test -p kabipay-payroll --test loan_recovery --offline --no-run }
        else { & cargo test -p kabipay-loans --test database --offline --no-run }
        if ($LASTEXITCODE -ne 0) { throw 'Financial integration test compilation failed' }
    } finally { Pop-Location }
    $schemaSql = & py -3 (Join-Path $databaseRoot 'tests/migrations/loan_fixture_sql.py')
    if ($LASTEXITCODE -ne 0) { throw 'Loan schema fixture generation failed' }
    $runtimeSql = & py -3 (Join-Path $PSScriptRoot 'loan_runtime_fixture.py')
    if ($LASTEXITCODE -ne 0) { throw 'Upstream fixture generation failed' }
    # All Loan commands now participate in exit coordination. Apply its actual schema
    # even for the Loans-only suite, with the required upstream Finance adapters.
    $payrollSql = & py -3 (Join-Path $PSScriptRoot 'payroll_loan_fixture.py')
    if ($LASTEXITCODE -ne 0) { throw 'Finance fixture generation failed' }
    $runtimeSql += $payrollSql
    [IO.File]::WriteAllText($fixturePath, ($schemaSql -join "`n") + "`n" + ($runtimeSql -join "`n"), [Text.UTF8Encoding]::new($false))
    & docker run --rm -d --name $containerName -p 127.0.0.1::5432 -e POSTGRES_USER=loan_fixture -e POSTGRES_PASSWORD=loan_fixture -e POSTGRES_DB=loan_phase_a postgres:16-alpine
    if ($LASTEXITCODE -ne 0) { throw 'Disposable PostgreSQL startup failed' }
    $created = $true
    $ready = $false
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        & docker exec $containerName pg_isready -U loan_fixture -d loan_phase_a | Out-Null
        if ($LASTEXITCODE -eq 0) { $ready = $true; break }
        Start-Sleep -Milliseconds 500
    }
    if (!$ready) { throw 'Disposable PostgreSQL did not become ready' }
    & docker cp $fixturePath "${containerName}:/tmp/loan-fixture.sql"
    if ($LASTEXITCODE -ne 0) { throw 'Fixture copy failed' }
    & docker exec $containerName psql -q -U loan_fixture -d loan_phase_a -f /tmp/loan-fixture.sql
    if ($LASTEXITCODE -ne 0) { throw 'Fixture SQL failed' }
    $binding = (& docker port $containerName 5432/tcp).Trim()
    if ($binding -notmatch '^127\.0\.0\.1:(\d+)$') { throw 'Unexpected test database binding' }
    $env:LOAN_TEST_DATABASE_URL = "postgresql://loan_fixture:loan_fixture@127.0.0.1:$($Matches[1])/loan_phase_a"
    Push-Location $serviceRoot
    try {
        if ($Exit) { & cargo test -p kabipay-employee --test exit_finance --offline -- --ignored --test-threads=1 }
        elseif ($Payroll) { & cargo test -p kabipay-payroll --test loan_recovery --offline -- --ignored --test-threads=1 }
        else { & cargo test -p kabipay-loans --test database --offline -- --ignored --test-threads=1 }
        if ($LASTEXITCODE -ne 0) { throw 'Loan database tests failed' }
    }
    finally { Pop-Location }
} finally {
    $env:LOAN_TEST_DATABASE_URL = $previousUrl
    if ($created) { & docker stop $containerName }
    if (Test-Path -LiteralPath $fixturePath) { Remove-Item -LiteralPath $fixturePath }
    $env:TEMP = $previousTemp
    $env:TMP = $previousTmp
}
