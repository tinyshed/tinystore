param(
    [string]$Corpus = (Join-Path $env:TEMP 'tinystore-tsbs-run'),
    [int]$Scale = 20,
    [string]$Start = '2026-01-01T00:00:00Z',
    [string]$End = '2026-01-01T07:00:00Z',
    [switch]$SkipEngines,
    [switch]$Prototype
)
$ErrorActionPreference = 'Stop'

# the generator is pinned, because a corpus that moves is not a corpus
$revision = '8323e59c74027b108f4ad5ec5d3e498b0101a02e'
$vmImage = 'victoriametrics/victoria-metrics@sha256:86ca5fdb6d87d56ba047b044039019ba2bd9042b36e35f6ea34e437b6c825cef'
$promImage = 'prom/prometheus:v3.6.0'
$repo = Split-Path $PSScriptRoot -Parent
New-Item -ItemType Directory -Force $Corpus | Out-Null
$Corpus = (Resolve-Path $Corpus).Path
# docker gets argument arrays below, not an inline argument list: inline,
# PowerShell splits -in=/out/x.csv into -in=/out/x and .csv
$goCache = @('-v', 'tinystore-gocache:/go', '-e', 'GOCACHE=/go/build-cache', '-e', 'GOMODCACHE=/go/mod-cache')

if (!(Test-Path (Join-Path $Corpus 'tsbs-devops.lp'))) {
    Write-Output '== generating the TSBS DevOps corpus =='
    docker run --rm @goCache -v "${Corpus}:/out" -w /tmp golang:1.27 sh -c @"
git clone --depth 1 https://github.com/timescale/tsbs /tmp/tsbs >/dev/null 2>&1 &&
cd /tmp/tsbs && git checkout -q $revision &&
go build -o /tmp/gen ./cmd/tsbs_generate_data &&
/tmp/gen --use-case=devops --seed=123 --scale=$Scale --log-interval=10s \
  --timestamp-start=$Start --timestamp-end=$End \
  --format=influx --file=/out/tsbs-devops.lp
"@
    if ($LASTEXITCODE) { throw 'TSBS generation failed' }
}

Write-Output '== converting once, so every engine gets the same samples =='
$convertArgs = @('run', '--rm') + $goCache + @(
    '-v', "${repo}:/src", '-v', "${Corpus}:/out", '-w', '/src/bench/tsbs', 'golang:1.27',
    'go', 'run', '.',
    '-in=/out/tsbs-devops.lp', '-corpus=/out/series.jsonl', '-openmetrics=/out/prom.om')
$conversion = & docker @convertArgs
if ($LASTEXITCODE) { throw 'Conversion failed' }
Write-Output $conversion
$matched = [regex]::Match(($conversion -join "`n"), 'samples=(\d+)')
if (!$matched.Success) { throw 'Could not read the sample count from the conversion' }
$samples = [int]$matched.Groups[1].Value

Write-Output '== TinyStore =='
$package = './metrics'
$test = '^TestCorpusThroughPublicStore$'
if ($Prototype) { $package = './spike'; $test = '^TestTSBSCorpus$' }
docker run --rm -v "${repo}:/src" -v "${Corpus}:/corpus" -v tinystore-gocache:/go -w /src `
    -e TINYSTORE_JSONL=/corpus/series.jsonl -e GOCACHE=/go/build-cache golang:1.27 `
    go test $package -run $test -v -count=1 -timeout 60m
if ($LASTEXITCODE) { throw 'TinyStore measurement failed' }
if ($SkipEngines) { return }

Write-Output '== VictoriaMetrics =='
$vmData = Join-Path $Corpus 'vm-data'
if (Test-Path $vmData) { Remove-Item -Recurse -Force -LiteralPath $vmData }
docker rm -f tinystore-vm-tsbs 2>&1 | Out-Null
docker run -d --name tinystore-vm-tsbs -p '127.0.0.1:18429:8428' -v "${Corpus}:/corpus" $vmImage `
    '-storageDataPath=/corpus/vm-data' '-retentionPeriod=100y' '-precisionBits=64' `
    '-dedup.minScrapeInterval=0s' '-memory.allowedBytes=536870912' '-storage.minFreeDiskSpaceBytes=1048576' | Out-Null
if ($LASTEXITCODE) { throw 'VictoriaMetrics failed to start' }
try {
    $ready = $false
    for ($i = 0; $i -lt 60; $i++) {
        try { Invoke-WebRequest http://127.0.0.1:18429/health -TimeoutSec 2 | Out-Null; $ready = $true; break } catch { Start-Sleep -Seconds 1 }
    }
    if (!$ready) { throw 'VictoriaMetrics did not become ready' }
    curl.exe -s -f -X POST --data-binary "@$Corpus/series.jsonl" http://127.0.0.1:18429/api/v1/import
    if ($LASTEXITCODE) { throw 'VictoriaMetrics import failed' }
    Invoke-WebRequest -Method Post http://127.0.0.1:18429/internal/force_flush -TimeoutSec 300 | Out-Null
    # exported before the merge, so the bits are checked on what was stored
    curl.exe -s -f -G --data-urlencode 'match[]={__name__!=""}' `
        --data-urlencode 'start=2025-12-31T00:00:00Z' --data-urlencode 'end=2026-01-02T00:00:00Z' `
        --data-urlencode 'reduce_mem_usage=1' -o "$Corpus/vm-export.jsonl" http://127.0.0.1:18429/api/v1/export
    if ($LASTEXITCODE) { throw 'VictoriaMetrics export failed' }
    Invoke-WebRequest -Method Post 'http://127.0.0.1:18429/internal/force_merge?partition_prefix=' -TimeoutSec 900 | Out-Null
    Start-Sleep -Seconds 20
} finally {
    docker stop -t 120 tinystore-vm-tsbs | Out-Null
    docker rm tinystore-vm-tsbs | Out-Null
}
docker run --rm -m 8g -v "${repo}:/src" -v "${Corpus}:/corpus" -v tinystore-gocache:/go -w /src `
    -e TINYSTORE_JSONL=/corpus/series.jsonl -e TINYSTORE_ENGINE_EXPORT=/corpus/vm-export.jsonl `
    -e GOCACHE=/go/build-cache golang:1.27 `
    go test ./spike -run '^TestEngineExportIsBitExact$' -v -count=1 -timeout 60m
if ($LASTEXITCODE) { throw 'VictoriaMetrics exactness check failed' }

Write-Output '== Prometheus =='
$promData = Join-Path $Corpus 'prom-one'
if (Test-Path $promData) { Remove-Item -Recurse -Force -LiteralPath $promData }
New-Item -ItemType Directory -Force $promData | Out-Null
# one block chosen explicitly: its own compactor would also scrape itself, and
# its metrics grow the directory while it is being measured
docker run --rm -v "${Corpus}:/corpus" --entrypoint promtool $promImage `
    tsdb create-blocks-from openmetrics --max-block-duration=24h /corpus/prom.om /corpus/prom-one
if ($LASTEXITCODE) { throw 'Prometheus backfill failed' }
New-Item -ItemType Directory -Force (Join-Path $promData 'wal') | Out-Null
docker run --rm -v "${Corpus}:/corpus" --entrypoint sh $promImage -c 'promtool tsdb dump /corpus/prom-one > /corpus/prom-dump.txt'
if ($LASTEXITCODE) { throw 'Prometheus dump failed' }
docker run --rm -m 8g -v "${repo}:/src" -v "${Corpus}:/corpus" -v tinystore-gocache:/go -w /src `
    -e TINYSTORE_JSONL=/corpus/series.jsonl -e TINYSTORE_PROM_DUMP=/corpus/prom-dump.txt `
    -e GOCACHE=/go/build-cache golang:1.27 `
    go test ./spike -run '^TestEngineExportIsBitExact$' -v -count=1 -timeout 60m
if ($LASTEXITCODE) { throw 'Prometheus exactness check failed' }

Write-Output '== complete measured sizes =='
foreach ($engine in @(
        @{ Name = 'VictoriaMetrics directory'; Path = $vmData },
        @{ Name = 'Prometheus one block'; Path = $promData })) {
    $files = Get-ChildItem -LiteralPath $engine.Path -File -Recurse
    $bytes = ($files | Measure-Object Length -Sum).Sum
    $perSample = if ($samples) { [math]::Round($bytes / $samples, 4) } else { 'set samples' }
    Write-Output ("{0,-28} {1,10} bytes  {2,5} files  {3} B/sample" -f $engine.Name, $bytes, $files.Count, $perSample)
}
Write-Output 'TinyStore''s file size is in the selected test output above; -Prototype selects the historical research layout.'
