$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path
$overlayPath = [System.IO.Path]::GetTempFileName()
$priorGoWork = $env:GOWORK
try {
    $replacements = @{}
    $replacements[(Join-Path $repoRoot 'metrics/audit_probe_test.go')] = Join-Path $PSScriptRoot 'metrics_test.go.txt'
    $replacements[(Join-Path $repoRoot 'internal/sqlite/audit_probe_test.go')] = Join-Path $PSScriptRoot 'sqlite_test.go.txt'
    @{ Replace = $replacements } | ConvertTo-Json | Set-Content -LiteralPath $overlayPath -Encoding utf8
    $env:GOWORK = 'off'
    go -C $repoRoot test -overlay $overlayPath ./metrics ./internal/sqlite -run '^TestAudit' -v -count=1
    $auditExit = $LASTEXITCODE
} finally {
    $env:GOWORK = $priorGoWork
    Remove-Item -LiteralPath $overlayPath
}
exit $auditExit
