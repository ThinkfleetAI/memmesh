# MemMesh installer for Windows — downloads a prebuilt binary (no Rust needed).
#
#   irm https://memmesh.ai/install.ps1 | iex
#
# Env overrides:
#   $env:MEMMESH_VERSION = "v0.1.2"   # install a specific tag (default: latest)
#   $env:MEMMESH_BIN_DIR = "C:\tools" # install location (default: %LOCALAPPDATA%\memmesh\bin)

$ErrorActionPreference = "Stop"
$Repo = "ThinkfleetAI/memmesh"
$Bin  = "memmesh"

$version = if ($env:MEMMESH_VERSION) { $env:MEMMESH_VERSION } else { "latest" }
$binDir  = if ($env:MEMMESH_BIN_DIR) { $env:MEMMESH_BIN_DIR } else { "$env:LOCALAPPDATA\memmesh\bin" }

# --- detect arch -> Rust target triple ---
$arch = $env:PROCESSOR_ARCHITECTURE
switch ($arch) {
  "AMD64" { $target = "x86_64-pc-windows-msvc" }
  "ARM64" { $target = "aarch64-pc-windows-msvc" }
  default { throw "unsupported architecture '$arch'" }
}

if ($version -eq "latest") {
  $url = "https://github.com/$Repo/releases/latest/download/$Bin-$target.zip"
} else {
  $url = "https://github.com/$Repo/releases/download/$version/$Bin-$target.zip"
}

New-Item -ItemType Directory -Force -Path $binDir | Out-Null
$tmp = Join-Path $env:TEMP "memmesh-$(Get-Random)"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

Write-Host ":: downloading memmesh ($target, $version)" -ForegroundColor Cyan
try {
  Invoke-WebRequest -Uri $url -OutFile "$tmp\memmesh.zip" -UseBasicParsing
} catch {
  throw "download failed: $url  (has a release with binaries been published?)"
}

Expand-Archive -Path "$tmp\memmesh.zip" -DestinationPath $tmp -Force
$exe = Get-ChildItem -Path $tmp -Recurse -Filter "$Bin.exe" | Select-Object -First 1
if (-not $exe) { throw "archive did not contain $Bin.exe" }
Copy-Item $exe.FullName "$binDir\$Bin.exe" -Force
Remove-Item -Recurse -Force $tmp
Write-Host ":: installed to $binDir\$Bin.exe" -ForegroundColor Cyan

# --- add to the user PATH if missing ---
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$binDir*") {
  [Environment]::SetEnvironmentVariable("Path", "$userPath;$binDir", "User")
  Write-Host ":: added $binDir to your user PATH (restart your shell to pick it up)" -ForegroundColor Yellow
}

Write-Host ""
& "$binDir\$Bin.exe" --version
Write-Host ""
Write-Host "Next — wire it into your AI tools (Claude Code, Cursor, Codex):" -ForegroundColor Green
Write-Host "    $binDir\$Bin.exe install"
Write-Host ""
Write-Host "That adds the MCP server + auto-observe hook. Then restart your tool."
