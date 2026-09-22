param(
    [string]$Corpus = (Join-Path $env:TEMP 'tinystore-alibaba'),
    [int]$Every = 16,
    [int64]$Until = 172800,
    [string]$DumpFrom = '2026-01-01T00:00:00Z',
    [string]$DumpTo = '2026-01-01T02:00:00Z',
    [switch]$SkipEngines,
    [switch]$Prototype
)
$ErrorActionPreference = 'Stop'

# the archive is pinned by hash, because a corpus that moves is not a corpus
$archiveUrl = 'http://clusterdata2018pubcn.oss-cn-beijing.aliyuncs.com/machine_usage.tar.gz'
$archiveHash = '3e6ee87fd204bb85b9e234c5c75a5096580fdabc8f085b224033080090753a7a'
$vmImage = 'victoriametrics/victoria-metrics@sha256:86ca5fdb6d87d56ba047b044039019ba2bd9042b36e35f6ea34e437b6c825cef'
$promImage = 'prom/prometheus:v3.6.0'
$repo = Split-Path $PSScriptRoot -Parent
New-Item -ItemType Directory -Force $Corpus | Out-Null
$Corpus = (Resolve-Path $Corpus).Path
# docker gets argument arrays below, not an inline argument list: inline,
# PowerShell splits -in=/out/x.csv into -in=/out/x and .csv
$goCache = @('-v', 'dashbin-gocache:/go', '-e', 'GOCACHE=/go/build-cache', '-e', 'GOMODCACHE=/go/mod-cache')

$archive = Join-Path $Corpus 'machine_usage.tar.gz'
if (!(Test-Path $archive)) {
    Write-Output '== downloading the Alibaba cluster trace, about 1.8 GB =='
    curl.exe -f --retry 3 -o $archive $archiveUrl
    if ($LASTEXITCODE) { throw 'Download failed' }
}
$actual = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLower()
if ($actual -ne $archiveHash) { throw "Archive checksum mismatch: $actual" }

$csv = Join-Path $Corpus 'machine_usage.csv'
if (!(Test-Path $csv)) {
    Write-Output '== extracting =='
    tar -xzf $archive -C $Corpus
    if ($LASTEXITCODE) { throw 'Extraction failed' }
}

Write-Output "== selecting every ${Every}th machine, trace seconds below $Until =="
$selectArgs = @('run', '--rm') + $goCache + @(
    '-v', "${repo}:/src", '-v', "${Corpus}:/out", '-w', '/src/bench/alibaba', 'golang:1.27',
    'go', 'run', '.', '-in=/out/machine_usage.csv', '-out=/out/alibaba.lp',
    "-every=$Every", "-until=$Until")
$selection = & docker @selectArgs
if ($LASTEXITCODE) { throw 'Selection failed' }
Write-Output $selection

Write-Output '== converting once, so every engine gets the same samples =='
$convertArgs = @('run', '--rm') + $goCache + @(
    '-v', "${repo}:/src", '-v', "${Corpus}:/out", '-w', '/src/bench/tsbs', 'golang:1.27',
    'go', 'run', '.', '-in=/out/alibaba.lp', '-corpus=/out/series.jsonl', '-openmetrics=/out/prom.om')
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
docker run --rm -m 12g -v "${repo}:/src" -v "${Corpus}:/corpus" -v dashbin-gocache:/go -w /src `
    -e TINYSTORE_JSONL=/corpus/series.jsonl -e GOCACHE=/go/build-cache golang:1.27 `
    go test $package -run $test -v -count=1 -timeout 120m
if ($LASTEXITCODE) { throw 'TinyStore measurement failed' }
if ($SkipEngines) { return }

Write-Output '== VictoriaMetrics =='
$vmData = Join-Path $Corpus 'vm-data'
if (Test-Path $vmData) { Remove-Item -Recurse -Force -LiteralPath $vmData }
docker rm -f tinystore-vm-alibaba 2>&1 | Out-Null
docker run -d --name tinystore-vm-alibaba -p '127.0.0.1:18430:8428' -v "${Corpus}:/corpus" $vmImage `
    '-storageDataPath=/corpus/vm-data' '-retentionPeriod=100y' '-precisionBits=64' `
    '-dedup.minScrapeInterval=0s' '-memory.allowedBytes=1073741824' '-storage.minFreeDiskSpaceBytes=1048576' | Out-Null
if ($LASTEXITCODE) { throw 'VictoriaMetrics failed to start' }
try {
    $ready = $false
    for ($i = 0; $i -lt 60; $i++) {
        try { Invoke-WebRequest http://127.0.0.1:18430/health -TimeoutSec 2 | Out-Null; $ready = $true; break } catch { Start-Sleep -Seconds 1 }
    }
    if (!$ready) { throw 'VictoriaMetrics did not become ready' }
    curl.exe -s -f -X POST --data-binary "@$Corpus/series.jsonl" http://127.0.0.1:18430/api/v1/import
    if ($LASTEXITCODE) { throw 'VictoriaMetrics import failed' }
    Invoke-WebRequest -Method Post http://127.0.0.1:18430/internal/force_flush -TimeoutSec 900 | Out-Null
    curl.exe -s -f -G --data-urlencode 'match[]={__name__!=""}' `
        --data-urlencode 'start=2025-12-31T00:00:00Z' --data-urlencode 'end=2026-01-05T00:00:00Z' `
        --data-urlencode 'reduce_mem_usage=1' -o "$Corpus/vm-export.jsonl" http://127.0.0.1:18430/api/v1/export
    if ($LASTEXITCODE) { throw 'VictoriaMetrics export failed' }
    Invoke-WebRequest -Method Post 'http://127.0.0.1:18430/internal/force_merge?partition_prefix=' -TimeoutSec 1800 | Out-Null
    Start-Sleep -Seconds 30
} finally {
    docker stop -t 300 tinystore-vm-alibaba | Out-Null
    docker rm tinystore-vm-alibaba | Out-Null
}
docker run --rm -m 12g -v "${repo}:/src" -v "${Corpus}:/corpus" -v dashbin-gocache:/go -w /src `
    -e TINYSTORE_JSONL=/corpus/series.jsonl -e TINYSTORE_ENGINE_EXPORT=/corpus/vm-export.jsonl `
    -e GOCACHE=/go/build-cache golang:1.27 `
    go test ./spike -run '^TestEngineExportIsBitExact$' -v -count=1 -timeout 120m
if ($LASTEXITCODE) { throw 'VictoriaMetrics exactness check failed' }

Write-Output '== Prometheus =='
$promData = Join-Path $Corpus 'prom-one'
if (Test-Path $promData) { Remove-Item -Recurse -Force -LiteralPath $promData }
New-Item -ItemType Directory -Force $promData | Out-Null
docker run --rm -v "${Corpus}:/corpus" --entrypoint promtool $promImage `
    tsdb create-blocks-from openmetrics --max-block-duration=72h /corpus/prom.om /corpus/prom-one
if ($LASTEXITCODE) { throw 'Prometheus backfill failed' }
New-Item -ItemType Directory -Force (Join-Path $promData 'wal') | Out-Null
# a whole dump of this corpus is tens of gigabytes of text, so the bits are
# checked on a stated window and the window is named in the result
$from = [int64]([datetimeoffset]::Parse($DumpFrom).ToUnixTimeMilliseconds())
$to = [int64]([datetimeoffset]::Parse($DumpTo).ToUnixTimeMilliseconds())
docker run --rm -v "${Corpus}:/corpus" --entrypoint sh $promImage `
    -c "promtool tsdb dump --min-time=$from --max-time=$to /corpus/prom-one > /corpus/prom-dump.txt"
if ($LASTEXITCODE) { throw 'Prometheus dump failed' }
docker run --rm -m 12g -v "${repo}:/src" -v "${Corpus}:/corpus" -v dashbin-gocache:/go -w /src `
    -e TINYSTORE_JSONL=/corpus/series.jsonl -e TINYSTORE_PROM_DUMP=/corpus/prom-dump.txt `
    -e TINYSTORE_WINDOW="$from-$to" -e GOCACHE=/go/build-cache golang:1.27 `
    go test ./spike -run '^TestEngineExportIsBitExact$' -v -count=1 -timeout 120m
if ($LASTEXITCODE) { throw 'Prometheus exactness check failed' }

Write-Output '== complete measured sizes =='
foreach ($engine in @(
        @{ Name = 'VictoriaMetrics directory'; Path = $vmData },
        @{ Name = 'Prometheus one block'; Path = $promData })) {
    $files = Get-ChildItem -LiteralPath $engine.Path -File -Recurse
    $bytes = ($files | Measure-Object Length -Sum).Sum
    Write-Output ("{0,-28} {1,12} bytes  {2,5} files  {3} B/sample" -f `
            $engine.Name, $bytes, $files.Count, [math]::Round($bytes / $samples, 4))
}
Write-Output 'TinyStore''s own file size is in the TestTSBSCorpus output above.'
