# Startet Datenbank (falls nötig) und Server auf diesem Rechner.
# -Open: Browser öffnen, sobald der Server bereit ist.
param([switch]$Open)
$ErrorActionPreference = "Stop"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:USERPROFILE\llvm-mingw\llvm-mingw-20261006-ucrt-x86_64\bin;$env:PATH"
$url = "http://localhost:3000"

function Test-Server {
    try { Invoke-WebRequest "$url/login" -UseBasicParsing -TimeoutSec 2 | Out-Null; $true } catch { $false }
}

# Läuft schon? Dann nur den Browser öffnen.
if (Test-Server) {
    Write-Host "CallCenter läuft bereits: $url"
    if ($Open) { Start-Process $url }
    exit 0
}

$pg = "$env:USERPROFILE\pgsql17\pgsql\bin"
if (Test-Path $pg) {
    & "$pg\pg_isready.exe" -h localhost -p 5432 -q
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Starte Datenbank ..."
        Start-Process -FilePath "$pg\pg_ctl.exe" -ArgumentList "-D `"$env:USERPROFILE\pgsql17\data`" -l `"$env:USERPROFILE\pgsql17\pg.log`" start" -NoNewWindow -Wait
    }
}

if ($Open) {
    # Wartet im Hintergrund, bis der Server antwortet (erster Build kann dauern).
    Start-Job -ArgumentList $url -ScriptBlock {
        param($url)
        for ($i = 0; $i -lt 300; $i++) {
            try { Invoke-WebRequest "$url/login" -UseBasicParsing -TimeoutSec 2 | Out-Null; Start-Process $url; return } catch { Start-Sleep 2 }
        }
    } | Out-Null
}

Set-Location $PSScriptRoot
Write-Host "Starte CallCenter ($url) ..."
cargo run @args
