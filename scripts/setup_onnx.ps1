# setup_onnx.ps1 — install what Dubplate needs for stem separation on Windows.
#
#   powershell -ExecutionPolicy Bypass -File scripts\setup_onnx.ps1          model + DirectML runtime (any modern GPU)
#   powershell -ExecutionPolicy Bypass -File scripts\setup_onnx.ps1 -Cpu     model + CPU-only runtime
#   ... -NoModel / -NoRuntime                                                 install only one of the two
#
# Everything lands in %LOCALAPPDATA%\Dubplate\ :
#   models\htdemucs-dml.onnx   HTDemucs 4-stem model (372 MB), checked against a pinned SHA-256
#   runtime\               onnxruntime.dll (plus DirectML.dll for the GPU build)
#
# DirectML runs on NVIDIA, AMD and Intel GPUs through DirectX 12; no CUDA install is needed.
# Nothing here has been run on a Windows machine yet - please report problems.
param(
    [switch]$Cpu,
    [switch]$NoModel,
    [switch]$NoRuntime
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is much faster without its progress bar

$ModelUrl    = 'https://huggingface.co/ferrite-stems/htdemucs-onnx-dml/resolve/main/htdemucs-dml.onnx'
$ModelSha256 = '9e06fef2b23e0f772c6a0cedc4da3aeacb066fa2fa7b6bf70ba08bb1db6220ab'
$OrtVersion  = '1.31.0'   # CPU runtime, from the ONNX Runtime releases on GitHub

$Data    = Join-Path $env:LOCALAPPDATA 'Dubplate'
$Models  = Join-Path $Data 'models'
$Runtime = Join-Path $Data 'runtime'
New-Item -ItemType Directory -Force -Path $Models, $Runtime | Out-Null

if (-not $NoModel) {
    $dest = Join-Path $Models 'htdemucs-dml.onnx'
    if ((Test-Path $dest) -and ((Get-FileHash $dest -Algorithm SHA256).Hash -eq $ModelSha256.ToUpper())) {
        Write-Host 'model: already installed and verified'
    } else {
        Write-Host 'model: downloading 372 MB ...'
        $part = "$dest.part"
        Invoke-WebRequest -Uri $ModelUrl -OutFile $part
        if ((Get-FileHash $part -Algorithm SHA256).Hash -ne $ModelSha256.ToUpper()) {
            Remove-Item $part -Force
            throw 'model: checksum mismatch - download discarded'
        }
        Move-Item $part $dest -Force
        Write-Host "model: installed -> $dest"
    }
}

if (-not $NoRuntime) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $work = Join-Path ([System.IO.Path]::GetTempPath()) ("dubplate-ort-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    try {
        if ($Cpu) {
            Write-Host "runtime: fetching CPU-only ONNX Runtime $OrtVersion ..."
            $zip = Join-Path $work 'ort.zip'
            Invoke-WebRequest -Uri "https://github.com/microsoft/onnxruntime/releases/download/v$OrtVersion/onnxruntime-win-x64-$OrtVersion.zip" -OutFile $zip
            [System.IO.Compression.ZipFile]::ExtractToDirectory($zip, $work)
            $lib = Get-ChildItem -Path $work -Recurse -Filter 'onnxruntime.dll' | Select-Object -First 1
            if (-not $lib) { throw 'runtime: no onnxruntime.dll found in the download' }
            Copy-Item $lib.FullName $Runtime -Force
            # The CPU zip keeps its providers library beside it.
            Get-ChildItem -Path $lib.DirectoryName -Filter 'onnxruntime_providers_shared.dll' -ErrorAction SilentlyContinue |
                Copy-Item -Destination $Runtime -Force
        } else {
            # The DirectML build is only published as a Python wheel (a zip with the DLLs inside).
            Write-Host 'runtime: fetching ONNX Runtime with DirectML (about 25 MB) ...'
            $meta = Invoke-RestMethod -Uri 'https://pypi.org/pypi/onnxruntime-directml/json'
            $file = $meta.urls | Where-Object { $_.filename -like '*win_amd64.whl' } | Select-Object -First 1
            if (-not $file) { throw 'runtime: no Windows build of onnxruntime-directml found on PyPI' }
            $whl = Join-Path $work 'ort.zip'
            Invoke-WebRequest -Uri $file.url -OutFile $whl
            if ((Get-FileHash $whl -Algorithm SHA256).Hash -ne $file.digests.sha256.ToUpper()) { throw 'runtime: checksum mismatch' }
            $dir = Join-Path $work 'wheel'
            [System.IO.Compression.ZipFile]::ExtractToDirectory($whl, $dir)
            $capi = Join-Path $dir 'onnxruntime\capi'
            foreach ($name in 'onnxruntime.dll', 'DirectML.dll', 'onnxruntime_providers_shared.dll') {
                $f = Join-Path $capi $name
                if (Test-Path $f) { Copy-Item $f $Runtime -Force }
            }
            if (-not (Test-Path (Join-Path $Runtime 'onnxruntime.dll'))) { throw 'runtime: no onnxruntime.dll found in the wheel' }
        }
        Write-Host "runtime: installed -> $Runtime"
    } finally {
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }
}

Write-Host ''
if ($Cpu) { Write-Host 'Done. Stems will use the CPU when you analyse a deck.' }
else      { Write-Host 'Done. Stems will use the GPU (DirectML) when you analyse a deck.' }
