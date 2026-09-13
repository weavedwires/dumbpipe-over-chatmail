$ErrorActionPreference = "Stop"

$VER = (Invoke-RestMethod https://api.github.com/repos/weavedwires/dumbpipe-over-chatmail/releases/latest).tag_name
$url = "https://github.com/weavedwires/dumbpipe-over-chatmail/releases/download/$VER/dumbpipe-$VER-dumbpipe-windows-x86_64.zip"

$tmp = Join-Path $env:TEMP "dumbpipe-install"
New-Item -ItemType Directory -Force $tmp | Out-Null

Invoke-WebRequest -Uri $url -OutFile (Join-Path $tmp "dumbpipe.zip")
Expand-Archive -Force (Join-Path $tmp "dumbpipe.zip") -DestinationPath $tmp

$bin = "$HOME\.local\bin"
New-Item -ItemType Directory -Force $bin | Out-Null
Move-Item -Force (Join-Path $tmp "dumbpipe.exe") "$bin\dumbpipe.exe"

$env:PATH = "$bin;$env:PATH"
setx PATH "$bin;$env:PATH" | Out-Null

Remove-Item -Recurse -Force $tmp
Write-Host "dumbpipe $VER installed to $bin"