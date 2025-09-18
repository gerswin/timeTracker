# Requires: PowerShell 5+ and Rust toolchain installed
param(
  [int]$Port = 49219,
  [switch]$Debug
)

$ErrorActionPreference = 'Stop'

function Invoke-Http($path){
  $base = "http://127.0.0.1:$Port"
  try { Invoke-RestMethod -Method GET -Uri ($base + $path) -TimeoutSec 5 }
  catch { Write-Host "[ERR] $path -> $($_.Exception.Message)" -ForegroundColor Red; return $null }
}

Write-Host "[win_smoke] building agent-daemon…"
cargo build @('--quiet') -p agent-daemon

Write-Host "[win_smoke] launching…"
$env:RUST_LOG = if($Debug){ 'debug' } else { 'info' }
$p = Start-Process -NoNewWindow -PassThru -FilePath target\debug\agent-daemon.exe
Start-Sleep -Milliseconds 500

try {
  for($i=0; $i -lt 50; $i++){
    $r = Invoke-Http '/healthz'
    if($r){ break }
    Start-Sleep -Milliseconds 200
  }
  Write-Host "[win_smoke] /healthz:"; Invoke-Http '/healthz' | ConvertTo-Json -Depth 5
  Write-Host "[win_smoke] /state:"; Invoke-Http '/state' | ConvertTo-Json -Depth 6
  Write-Host "[win_smoke] /queue:"; Invoke-Http '/queue' | ConvertTo-Json -Depth 6
  $sample = Invoke-Http '/debug/sample'
  if($sample){
    Write-Host "[win_smoke] /debug/sample:"; $sample | ConvertTo-Json -Depth 6
    if($sample.win_pid){
      Write-Host "[win_smoke] foco PID:" $sample.win_pid "HWND:" $sample.win_hwnd "estrategia:" $sample.title_source
    }
  }
}
finally {
  if($p -and !$p.HasExited){ Stop-Process -Id $p.Id -Force }
}

Write-Host "[win_smoke] done" -ForegroundColor Green

