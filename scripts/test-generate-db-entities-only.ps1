$ErrorActionPreference = 'Stop'

function Assert-True {
    param(
        [bool]$Condition,
        [string]$Message
    )

    if (-not $Condition) {
        throw $Message
    }
}

function Write-Utf8File {
    param(
        [string]$Path,
        [string]$Contents
    )

    [System.IO.File]::WriteAllText($Path, $Contents, [System.Text.UTF8Encoding]::new($false))
}

$generatorSource = Join-Path $PSScriptRoot 'generate_db_entities.py'
$pythonLauncher = Get-Command py -ErrorAction Stop
$temporaryRoot = [System.IO.Path]::GetTempPath()
$fixtureRoot = Join-Path $temporaryRoot ("hrms-generate-db-entities-only-" + [guid]::NewGuid().ToString('N'))

try {
    $fixtureGenerator = Join-Path $fixtureRoot 'hrms-svc\scripts\generate_db_entities.py'
    $fixtureMigrations = Join-Path $fixtureRoot 'hrms-database\changelog\migrations'
    $fixtureTenantEntities = Join-Path $fixtureRoot 'hrms-svc\crates\kabipay-db-entities\src\tenant'
    $legacyMigration = Join-Path $fixtureMigrations '0005_legacy'
    $targetMigration = Join-Path $fixtureMigrations '0063_target'

    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $fixtureGenerator), $legacyMigration, $targetMigration, $fixtureTenantEntities | Out-Null
    Copy-Item -LiteralPath $generatorSource -Destination $fixtureGenerator

    Write-Utf8File -Path (Join-Path $legacyMigration 'legacy.xml') -Contents @'
<?xml version="1.0" encoding="UTF-8"?>
<databaseChangeLog xmlns="http://www.liquibase.org/xml/ns/dbchangelog">
    <changeSet id="legacy" author="test">
        <createTable tableName="legacy_table" schemaName="${schema}">
            <column name="id" type="UUID"><constraints primaryKey="true" nullable="false"/></column>
        </createTable>
    </changeSet>
</databaseChangeLog>
'@
    Write-Utf8File -Path (Join-Path $targetMigration 'target.xml') -Contents @'
<?xml version="1.0" encoding="UTF-8"?>
<databaseChangeLog xmlns="http://www.liquibase.org/xml/ns/dbchangelog">
    <changeSet id="target" author="test">
        <createTable tableName="target_table" schemaName="${schema}">
            <column name="id" type="UUID"><constraints primaryKey="true" nullable="false"/></column>
            <column name="payload" type="JSONB"><constraints nullable="false"/></column>
        </createTable>
    </changeSet>
</databaseChangeLog>
'@

    Write-Utf8File -Path (Join-Path $fixtureTenantEntities 'prelude.rs') -Contents "fixture-prelude`n"
    Write-Utf8File -Path (Join-Path $fixtureTenantEntities 'd0005_legacy.rs') -Contents "fixture-legacy-module`n"
    $modBefore = "//! fixture modules`n`npub mod prelude;`npub use prelude::*;`n`npub mod d0005_legacy;`n"
    Write-Utf8File -Path (Join-Path $fixtureTenantEntities 'mod.rs') -Contents $modBefore

    $beforeFiles = @{}
    Get-ChildItem -LiteralPath $fixtureTenantEntities -File | ForEach-Object {
        $beforeFiles[$_.Name] = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash
    }

    & $pythonLauncher.Source -3 $fixtureGenerator --only 0063_target
    Assert-True ($LASTEXITCODE -eq 0) "targeted generator exited with $LASTEXITCODE"

    $targetEntityName = 'd0063_target.rs'
    $targetEntityPath = Join-Path $fixtureTenantEntities $targetEntityName
    Assert-True (Test-Path -LiteralPath $targetEntityPath) 'targeted generator did not create the requested module'
    $targetEntity = Get-Content -Raw -LiteralPath $targetEntityPath
    Assert-True ($targetEntity -match 'pub mod target_table') 'targeted generator did not emit the requested table'
    Assert-True ($targetEntity -match 'pub payload: Json,') 'targeted generator did not preserve target column types'

    foreach ($protectedName in @('prelude.rs', 'd0005_legacy.rs')) {
        $afterHash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $fixtureTenantEntities $protectedName)).Hash
        Assert-True ($afterHash -eq $beforeFiles[$protectedName]) "$protectedName changed during targeted generation"
    }

    $afterFiles = @(Get-ChildItem -LiteralPath $fixtureTenantEntities -File | ForEach-Object { $_.Name } | Sort-Object)
    $addedFiles = @($afterFiles | Where-Object { -not $beforeFiles.ContainsKey($_) })
    Assert-True ($addedFiles.Count -eq 1 -and $addedFiles[0] -eq $targetEntityName) 'targeted generation must create only the requested module'

    $expectedMod = "//! fixture modules`n`npub mod prelude;`npub use prelude::*;`n`npub mod d0005_legacy;`npub mod d0063_target;`n"
    $actualMod = Get-Content -Raw -LiteralPath (Join-Path $fixtureTenantEntities 'mod.rs')
    Assert-True ($actualMod -ceq $expectedMod) 'targeted generation must change mod.rs only by adding the sorted requested export'

    Write-Host 'Targeted entity generator fixture contract passed.'
}
finally {
    if (Test-Path -LiteralPath $fixtureRoot) {
        Assert-True ($fixtureRoot.StartsWith($temporaryRoot, [System.StringComparison]::OrdinalIgnoreCase)) 'refusing to remove a fixture outside the temporary directory'
        Remove-Item -LiteralPath $fixtureRoot -Recurse -Force
    }
}
