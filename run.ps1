# Startet Datenbank (falls nötig) und Server auf diesem Rechner.
$ErrorActionPreference = "Stop"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:USERPROFILE\llvm-mingw\llvm-mingw-20261006-ucrt-x86_64\bin;$env:PATH"

$pg = "$env:USERPROFILE\pgsql17\pgsql\bin"
if (Test-Path $pg) {
    & "$pg\pg_isready.exe" -h localhost -p 5432 -q
    if ($LASTEXITCODE -ne 0) {
        Start-Process -FilePath "$pg\pg_ctl.exe" -ArgumentList "-D `"$env:USERPROFILE\pgsql17\data`" -l `"$env:USERPROFILE\pgsql17\pg.log`" start" -NoNewWindow -Wait
    }
}

Set-Location $PSScriptRoot
cargo run @args
