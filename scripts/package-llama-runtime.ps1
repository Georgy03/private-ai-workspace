$ErrorActionPreference = "Stop"
if (-not $env:LLAMA_CPP_REF) { throw "Set LLAMA_CPP_REF to a reviewed llama.cpp commit SHA" }

$root = Split-Path -Parent $PSScriptRoot
$work = Join-Path $env:TEMP "enclave-llama-cpp"
$out = Join-Path $root "release/runtime/windows-x86_64"
Remove-Item -Force -Recurse -ErrorAction SilentlyContinue $work, $out
git clone --filter=blob:none https://github.com/ggml-org/llama.cpp.git $work
git -C $work checkout --detach $env:LLAMA_CPP_REF
cmake -S $work -B "$work/build" -G "Visual Studio 17 2022" -A x64 -DBUILD_SHARED_LIBS=OFF
cmake --build "$work/build" --config Release --target llama-cli
New-Item -ItemType Directory -Force $out | Out-Null
Copy-Item "$work/build/bin/Release/llama-cli.exe" "$out/llama-cli.exe"
Copy-Item "$work/LICENSE" "$out/LICENSE-llama.cpp"
Get-FileHash "$out/llama-cli.exe" -Algorithm SHA256 | ForEach-Object { "$($_.Hash.ToLower())  llama-cli.exe" } | Set-Content "$out/SHA256SUMS"
Write-Output "Packaged $out. Sign SHA256SUMS in the release pipeline before distribution."
