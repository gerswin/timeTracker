# Windows helper: configure .env and run the agent
# Usage examples:
#   powershell -ExecutionPolicy Bypass -File scripts\win_run.ps1 -PanelAddr 127.0.0.1:49220 -IdleActiveThresholdMs 300000 -Run -OpenUI

param(
  [string]$PanelAddr = '127.0.0.1:49219',
  [int]$IdleActiveThresholdMs = 60000,
  [string]$LogLevel = 'info',
  [switch]$Run,
  [switch]$Debug,
  [switch]$OpenUI,
  [switch]$Force
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$envPath = Join-Path $root '.env'

function Write-Section($label){ Write-Host "`n=== $label ===" -ForegroundColor Cyan }

function Save-Env {
  $lines = @()
  $lines += "PANEL_ADDR=$PanelAddr"
  $lines += "IDLE_ACTIVE_THRESHOLD_MS=$IdleActiveThresholdMs"
  if($IsMacOS){ $lines += "RIPOR_NO_AUTO_PROMPT=0" }
  $lines += "RUST_LOG=$LogLevel"
  $content = ($lines -join "`r`n") + "`r`n"
  if(Test-Path $envPath){
    if($Force){ Write-Host "[win_run] Overwriting .env (Force)" -ForegroundColor Yellow }
    else { Write-Host "[win_run] .env exists, use -Force to overwrite; skipping write" -ForegroundColor Yellow; return }
  }
  $content | Set-Content -Encoding UTF8 -Path $envPath
  Write-Host "[win_run] Wrote $envPath" -ForegroundColor Green
}

function Ensure-Agent {
  $exeDebug = Join-Path $root 'target\\debug\\agent-daemon.exe'
  if(-not (Test-Path $exeDebug)){
    Write-Section 'Building agent-daemon (debug)'
    cargo build -p agent-daemon | Out-Null
  }
}

function Run-Agent {
  $env:PANEL_ADDR = $PanelAddr
  $env:RUST_LOG = if($Debug){ 'debug' } else { $LogLevel }
  $cmd = Get-Command cargo -ErrorAction SilentlyContinue
  if($cmd){
    Write-Section 'Running with cargo'
    # Run without blocking terminal (CTRL+C will close)
    Start-Process -NoNewWindow -FilePath cargo -ArgumentList @('run','-p','agent-daemon') | Out-Null
  } else {
    Ensure-Agent
    $exe = Join-Path $root 'target\\debug\\agent-daemon.exe'
    Write-Section "Running $exe"
    Start-Process -NoNewWindow -FilePath $exe | Out-Null
  }
  if($OpenUI){ Start-Process "http://$PanelAddr/ui" | Out-Null }
}

Write-Section 'Saving .env'
Save-Env
if($Run){ Run-Agent }
Write-Host "[win_run] Done" -ForegroundColor Green

