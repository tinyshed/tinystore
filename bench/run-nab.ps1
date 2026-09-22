param([string]$Corpus = (Join-Path $env:TEMP 'tinystore-nab-ea702d75'), [switch]$Victoria)
$ErrorActionPreference = 'Stop'
$revision = 'ea702d75cc2258d9d7dd35ca8e5e2539d71f3140'
$repo = (Split-Path $PSScriptRoot -Parent).Replace('\','/')
New-Item -ItemType Directory -Force $Corpus | Out-Null
$Corpus = (Resolve-Path $Corpus).Path.Replace('\','/')
foreach ($line in Get-Content (Join-Path $PSScriptRoot 'nab-sha256.txt')) {
    $hash, $relative = $line -split '\s+', 2
    $path = Join-Path $Corpus $relative
    New-Item -ItemType Directory -Force (Split-Path $path -Parent) | Out-Null
    if (!(Test-Path $path)) {
        Invoke-WebRequest "https://raw.githubusercontent.com/numenta/NAB/$revision/data/$relative" -OutFile $path
    }
    if ((Get-FileHash $path -Algorithm SHA256).Hash.ToLower() -ne $hash) { throw "Checksum mismatch: $relative" }
}
docker run --rm -v "${repo}:/src" -v "${Corpus}:/corpus" -v tinystore-gocache:/go -w /src -e TINYSTORE_CORPUS=/corpus -e GOCACHE=/go/build-cache golang:1.27 go test ./spike -run '^TestRealCorpus$' -v -count=1
if ($LASTEXITCODE) { throw 'Corpus benchmark failed' }
if (!$Victoria) { return }
$suffix = [guid]::NewGuid().ToString('N').Substring(0,8)
$name = "tinystore-vm-$suffix"
docker run -d --name $name -p '127.0.0.1:18428:8428' -v "${Corpus}:/corpus" 'victoriametrics/victoria-metrics@sha256:86ca5fdb6d87d56ba047b044039019ba2bd9042b36e35f6ea34e437b6c825cef' "-storageDataPath=/corpus/vm-$suffix" '-retentionPeriod=100y' '-precisionBits=64' '-dedup.minScrapeInterval=0s' '-memory.allowedBytes=134217728' '-storage.minFreeDiskSpaceBytes=1048576'
if ($LASTEXITCODE) { throw 'VictoriaMetrics failed to start' }
try {
    $ready = $false
    for ($attempt=0; $attempt -lt 30; $attempt++) {
        try { Invoke-WebRequest http://127.0.0.1:18428/health | Out-Null; $ready=$true; break } catch { Start-Sleep -Seconds 1 }
    }
    if (!$ready) { throw 'VictoriaMetrics did not become ready' }
    Invoke-WebRequest -Method Post http://127.0.0.1:18428/api/v1/import -ContentType application/json -InFile (Join-Path $Corpus 'import.jsonl') | Out-Null
    Invoke-WebRequest -Method Post http://127.0.0.1:18428/internal/force_flush | Out-Null
    Invoke-WebRequest 'http://127.0.0.1:18428/api/v1/export?match%5B%5D=nab&start=2010-01-01T00%3A00%3A00Z&end=2020-01-01T00%3A00%3A00Z&reduce_mem_usage=1' -OutFile (Join-Path $Corpus 'export.jsonl')
    Invoke-WebRequest -Method Post 'http://127.0.0.1:18428/internal/force_merge?partition_prefix=' | Out-Null
    docker run --rm -v "${repo}:/src" -v "${Corpus}:/corpus" -v tinystore-gocache:/go -w /src -e TINYSTORE_CORPUS=/corpus -e TINYSTORE_VM_EXPORT=/corpus/export.jsonl -e GOCACHE=/go/build-cache golang:1.27 go test ./spike -run '^TestVictoriaCorpusRoundTrip$' -v -count=1
    if ($LASTEXITCODE) { throw 'VictoriaMetrics export validation failed' }
} finally {
    docker stop $name | Out-Null
    docker rm $name | Out-Null
}
$bytes = (Get-ChildItem -LiteralPath (Join-Path $Corpus "vm-$suffix") -File -Recurse | Measure-Object Length -Sum).Sum
Write-Output "VictoriaMetrics complete directory, logical file bytes: $bytes"
