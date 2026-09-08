# Starts all federated GraphQL subgraphs: unified ops on 4010, tenant modules 4013-4030.
# Requires: cargo build for each crate (use: cargo build -j 1 -p <crate> per crate if full workspace build OOMs on Windows).
#
# Connection budget: each process opens its own SeaORM/sqlx pool. A managed Postgres with
# max_connections ~20 will reject or stall new logins if 19 pools each try to open many
# connections at once. Unless you already set these in the parent shell, we default to
# tiny pools so the full subgraph grid can start (raise them for production / single-service).
if (-not $env:KABIPAY_DB_POOL_MAX) { $env:KABIPAY_DB_POOL_MAX = '1' }
if (-not $env:KABIPAY_TENANT_DB_POOL_MAX) { $env:KABIPAY_TENANT_DB_POOL_MAX = '1' }

$ErrorActionPreference = 'Stop'
$base = Split-Path -Parent $PSScriptRoot
$runs = @(
    @('kabipay-ops.exe', 'KABIPAY_OPS_PORT', '4010'),
    @('kabipay-employee.exe', 'KABIPAY_EMPLOYEE_PORT', '4013'),
    @('kabipay-leave.exe', 'KABIPAY_LEAVE_PORT', '4014'),
    @('kabipay-attendance.exe', 'KABIPAY_ATTENDANCE_PORT', '4015'),
    @('kabipay-payroll.exe', 'KABIPAY_PAYROLL_PORT', '4016'),
    @('kabipay-tax.exe', 'KABIPAY_TAX_PORT', '4017'),
    @('kabipay-benefits.exe', 'KABIPAY_BENEFITS_PORT', '4018'),
    @('kabipay-expense.exe', 'KABIPAY_EXPENSE_PORT', '4019'),
    @('kabipay-recruitment.exe', 'KABIPAY_RECRUITMENT_PORT', '4020'),
    @('kabipay-performance.exe', 'KABIPAY_PERFORMANCE_PORT', '4021'),
    @('kabipay-lms.exe', 'KABIPAY_LMS_PORT', '4022'),
    @('kabipay-succession.exe', 'KABIPAY_SUCCESSION_PORT', '4023'),
    @('kabipay-compensation.exe', 'KABIPAY_COMPENSATION_PORT', '4024'),
    @('kabipay-assets.exe', 'KABIPAY_ASSETS_PORT', '4025'),
    @('kabipay-grievance.exe', 'KABIPAY_GRIEVANCE_PORT', '4026'),
    @('kabipay-workflow.exe', 'KABIPAY_WORKFLOW_PORT', '4027'),
    @('kabipay-notification.exe', 'KABIPAY_NOTIFICATION_PORT', '4028'),
    @('kabipay-analytics.exe', 'KABIPAY_ANALYTICS_PORT', '4029'),
    @('kabipay-survey.exe', 'KABIPAY_SURVEY_PORT', '4030')
)
Get-Process -Name 'kabipay-*' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
foreach ($r in $runs) {
    $ex, $k, $v = $r
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = Join-Path $base ("target\debug\" + $ex)
    $psi.WorkingDirectory = $base
    $psi.UseShellExecute = $false
    $psi.EnvironmentVariables[$k] = $v
    [void][System.Diagnostics.Process]::Start($psi)
    # Small delay avoids a simultaneous TLS/auth stampede against small managed DBs.
    Start-Sleep -Milliseconds 250
}
Write-Host "Started $($runs.Count) subgraph processes (KABIPAY_DB_POOL_MAX=$($env:KABIPAY_DB_POOL_MAX)). Ops: 4010; tenant modules: 4013-4030."
