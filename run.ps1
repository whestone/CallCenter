# Startet Datenbank (falls nötig) und Server auf diesem Rechner.
# -Open: Browser öffnen, sobald der Server bereit ist.
param([switch]$Open)
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:USERPROFILE\llvm-mingw\llvm-mingw-20261006-ucrt-x86_64\bin;$env:PATH"
$url = "http://localhost:3000"
$port = 3000

function Test-Server {
    try { Invoke-WebRequest "$url/login" -UseBasicParsing -TimeoutSec 2 | Out-Null; $true } catch { $false }
}

function Stop-WithError($message) {
    Write-Host ""
    Write-Host "FEHLER: $message" -ForegroundColor Red
    exit 1
}

# Läuft schon? Dann nur den Browser öffnen.
if (Test-Server) {
    Write-Host "CallCenter läuft bereits: $url"
    if ($Open) { Start-Process $url }
    exit 0
}

# Port belegt, aber kein CallCenter dahinter?
$busy = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue
if ($busy) {
    $owner = (Get-Process -Id $busy[0].OwningProcess -ErrorAction SilentlyContinue).ProcessName
    if ($owner -ne "callcenter") {
        Stop-WithError "Port $port ist bereits durch '$owner' belegt. Programm beenden oder BIND_ADDR in der .env ändern."
    }
    # Ein CallCenter-Prozess hängt (startet gerade oder reagiert nicht) – kurz warten.
    Write-Host "CallCenter startet bereits, warte ..."
    for ($i = 0; $i -lt 30 -and -not (Test-Server); $i++) { Start-Sleep 1 }
    if (Test-Server) {
        if ($Open) { Start-Process $url }
        exit 0
    }
    Stop-WithError "Ein CallCenter-Prozess belegt Port $port, antwortet aber nicht. Im Task-Manager 'callcenter.exe' beenden und erneut starten."
}

# Datenbank (lokale Installation unter %USERPROFILE%\pgsql17)
$pg = "$env:USERPROFILE\pgsql17\pgsql\bin"
$pgData = "$env:USERPROFILE\pgsql17\data"
$pgLog = "$env:USERPROFILE\pgsql17\pg.log"
if (Test-Path $pg) {
    & "$pg\pg_isready.exe" -h localhost -p 5432 -q
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Starte Datenbank ..."
        # Eigenes (verstecktes) Fenster: Die Datenbank läuft weiter, auch wenn dieses Fenster
        # geschlossen wird, und wird so nicht unsauber beendet.
        Start-Process -FilePath "$pg\pg_ctl.exe" -ArgumentList "-D `"$pgData`" -l `"$pgLog`" start" -WindowStyle Hidden
        $ready = $false
        for ($i = 0; $i -lt 30; $i++) {
            Start-Sleep 1
            & "$pg\pg_isready.exe" -h localhost -p 5432 -q
            if ($LASTEXITCODE -eq 0) { $ready = $true; break }
        }
        if (-not $ready) {
            Write-Host "Letzte Zeilen aus $pgLog :"
            Get-Content $pgLog -Tail 15 -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "  $_" }
            Stop-WithError "Die Datenbank startet nicht (siehe oben)."
        }
        Write-Host "Datenbank bereit."
    }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Stop-WithError "Rust (cargo) wurde nicht gefunden. Erwartet unter $env:USERPROFILE\.cargo\bin."
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
Write-Host "(Beim ersten Start oder nach Updates wird erst kompiliert – das kann ein paar Minuten dauern.)"
cargo run @args
$code = $LASTEXITCODE
# Strg+C bzw. Fenster schließen ist ein normales Beenden, kein Fehler.
if ($code -eq -1073741510 -or $code -eq 3221225786 -or $code -eq 130) {
    Write-Host "CallCenter wurde beendet."
} elseif ($code -ne 0) {
    Stop-WithError "CallCenter wurde mit Fehlercode $code beendet. Die Ursache steht in den Zeilen darüber."
}
