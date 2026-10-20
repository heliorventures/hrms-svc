param([switch]$Prepared)
$ErrorActionPreference = 'Stop'
$fixtureName = 'hrms-bound-runtime-' + [Guid]::NewGuid().ToString('N')
$fixtureTenant = [Guid]::NewGuid().ToString()
$fixtureCreated = $false
$fixtureExit = 1
$savedUrl = [Environment]::GetEnvironmentVariable('KABIPAY_BOUND_RUNTIME_FIXTURE_URL', 'Process')
$savedTenant = [Environment]::GetEnvironmentVariable('KABIPAY_BOUND_RUNTIME_FIXTURE_TENANT', 'Process')
try {
    if (-not $Prepared) {
        cargo test -p kabipay-common --test bound_runtime_postgres --offline --no-run
        if ($LASTEXITCODE -ne 0) { throw 'Fixture test compilation failed' }
    }
    docker run --pull=never --rm -d --name $fixtureName --memory 256m --cpus 1 -p '127.0.0.1::5432' -e POSTGRES_USER=fixture_admin -e POSTGRES_PASSWORD=synthetic_fixture -e POSTGRES_DB=fixture_admin postgres:16-alpine | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Disposable PostgreSQL creation failed' }
    $fixtureCreated = $true
    $ready = $false
    for ($attempt = 0; $attempt -lt 60; $attempt++) {
        docker exec $fixtureName pg_isready -h 127.0.0.1 -U fixture_admin -d fixture_admin *> $null
        if ($LASTEXITCODE -eq 0) { $ready = $true; break }
        Start-Sleep -Milliseconds 200
    }
    if (-not $ready) { throw 'Disposable PostgreSQL readiness failed' }
    $fixtureBinding = docker port $fixtureName 5432/tcp
    if ($fixtureBinding -notmatch '^127\.0\.0\.1:(\d+)$') { throw 'Refusing non-loopback fixture binding' }
    $fixturePort = $Matches[1]
    $fixtureSql = @"
CREATE ROLE bound_a_service LOGIN PASSWORD 'synthetic_runtime_a';
CREATE ROLE bound_b_service LOGIN PASSWORD 'synthetic_runtime_b';
CREATE DATABASE bound_a;
CREATE DATABASE bound_b;
REVOKE CONNECT ON DATABASE bound_a FROM PUBLIC;
REVOKE CONNECT ON DATABASE bound_b FROM PUBLIC;
GRANT CONNECT ON DATABASE bound_a TO bound_a_service;
GRANT CONNECT ON DATABASE bound_b TO bound_b_service;
"@
    $fixtureSql | docker exec -i $fixtureName psql -v ON_ERROR_STOP=1 -U fixture_admin -d fixture_admin | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Disposable database roles failed' }
    $fixtureSql = @"
CREATE SCHEMA tenant_fixture;
CREATE TABLE tenant_fixture.binding_marker(tenant_id UUID NOT NULL);
INSERT INTO tenant_fixture.binding_marker VALUES ('$fixtureTenant');
GRANT USAGE ON SCHEMA tenant_fixture TO bound_a_service;
GRANT SELECT ON tenant_fixture.binding_marker TO bound_a_service;
CREATE SCHEMA kabipay_ops;
CREATE TABLE kabipay_ops.tenant_database (
 id UUID PRIMARY KEY, tenant_id UUID NOT NULL, db_type TEXT NOT NULL,
 db_host TEXT NOT NULL, db_name TEXT NOT NULL, schema_name TEXT NOT NULL,
 is_active BOOLEAN NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
INSERT INTO kabipay_ops.tenant_database (id,tenant_id,db_type,db_host,db_name,schema_name,is_active)
 VALUES (gen_random_uuid(),'$fixtureTenant','POSTGRES','127.0.0.1','bound_a','tenant_fixture',true);
GRANT USAGE ON SCHEMA kabipay_ops TO bound_a_service;
GRANT SELECT, UPDATE, INSERT, DELETE ON kabipay_ops.tenant_database TO bound_a_service;
"@
    $fixtureSql | docker exec -i $fixtureName psql -v ON_ERROR_STOP=1 -U fixture_admin -d bound_a | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Disposable database marker failed' }
    [Environment]::SetEnvironmentVariable('KABIPAY_BOUND_RUNTIME_FIXTURE_URL', "postgres://bound_a_service:synthetic_runtime_a@127.0.0.1:$fixturePort/bound_a?sslmode=disable", 'Process')
    [Environment]::SetEnvironmentVariable('KABIPAY_BOUND_RUNTIME_FIXTURE_TENANT', $fixtureTenant, 'Process')
    cargo test -p kabipay-common --test bound_runtime_postgres --offline -- --ignored
    $fixtureExit = $LASTEXITCODE
} finally {
    [Environment]::SetEnvironmentVariable('KABIPAY_BOUND_RUNTIME_FIXTURE_URL', $savedUrl, 'Process')
    [Environment]::SetEnvironmentVariable('KABIPAY_BOUND_RUNTIME_FIXTURE_TENANT', $savedTenant, 'Process')
    if ($fixtureCreated -and $fixtureName.StartsWith('hrms-bound-runtime-')) { docker rm -f $fixtureName | Out-Null }
}
exit $fixtureExit
