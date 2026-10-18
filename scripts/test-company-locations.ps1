<# Uses synthetic data only. No repository .env or production configuration is loaded. #>
param()
$ErrorActionPreference='Stop'
$root=(Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$container='hrms-company-location-test-'+[guid]::NewGuid().ToString('N')
$created=$false
$previous=[Environment]::GetEnvironmentVariable('COMPANY_LOCATION_TEST_DATABASE_URL','Process')
function Assert-Exit([string]$Action){if($LASTEXITCODE -ne 0){throw "$Action failed (exit $LASTEXITCODE)"}}
try {
    docker run --pull=never --rm --detach --name $container --memory 256m --cpus 1 -p 127.0.0.1::5432 -e POSTGRES_USER=location_fixture -e POSTGRES_PASSWORD=location_fixture -e POSTGRES_DB=company_location_regression postgres:16-alpine
    Assert-Exit 'Start disposable PostgreSQL';$created=$true
    $ready=$false
    for($attempt=0;$attempt -lt 60;$attempt++){
        docker exec $container pg_isready -U location_fixture -d company_location_regression | Out-Null
        if($LASTEXITCODE -eq 0){$ready=$true;break}
        Start-Sleep -Milliseconds 500
    }
    if(-not $ready){throw 'Disposable PostgreSQL readiness timed out'}
    $binding=(docker port $container 5432/tcp).Trim();Assert-Exit 'Read fixture endpoint'
    if($binding -notmatch '^127\.0\.0\.1:(\d+)$'){throw 'Refused non-loopback test endpoint'}
    $env:COMPANY_LOCATION_TEST_DATABASE_URL="postgresql://location_fixture:location_fixture@127.0.0.1:$($Matches[1])/company_location_regression"
    Push-Location $root
    try {
        cargo test --offline -p kabipay-employee --test company_locations -- --ignored --test-threads=1
        Assert-Exit 'Company location PostgreSQL regression tests'
    }finally{Pop-Location}
}finally{
    [Environment]::SetEnvironmentVariable('COMPANY_LOCATION_TEST_DATABASE_URL',$previous,'Process')
    if($created){
        if($container -notmatch '^hrms-company-location-test-[a-f0-9]{32}$'){throw 'Owned container cleanup guard failed'}
        docker stop $container | Out-Null
        Assert-Exit 'Remove owned fixture container'
    }
}
