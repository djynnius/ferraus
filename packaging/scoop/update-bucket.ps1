# Update the scoop manifest in djynnius/scoop-bucket for a release tag (run by CI only).
# -DryRun prints the manifest instead of pushing it.
param([Parameter(Mandatory = $true)][string]$Version, [switch]$DryRun)
$ErrorActionPreference = "Stop"
$v = $Version.TrimStart("v")
$base = "https://github.com/djynnius/ferraus/releases/download/v$v"
$auto = "https://github.com/djynnius/ferraus/releases/download/v`$version"

# GitHub serves .sha256 files as application/octet-stream, which PowerShell 7 returns as bytes,
# so read them through a file
function Get-Sha([string]$name) {
  $tmp = New-TemporaryFile
  try {
    Invoke-WebRequest "$base/$name.sha256" -OutFile $tmp
    return (Get-Content $tmp -Raw).Trim().Split(" ")[0]
  } catch {
    return $null
  } finally {
    Remove-Item $tmp -ErrorAction SilentlyContinue
  }
}

$arch = [ordered]@{}
$autoArch = [ordered]@{}
foreach ($a in @(@{ key = "64bit"; target = "x86_64-pc-windows-msvc" }, @{ key = "arm64"; target = "aarch64-pc-windows-msvc" })) {
  $zip = "ferraus-$($a.target).zip"
  $sha = Get-Sha $zip
  if (-not $sha) {
    if ($a.key -eq "64bit") { throw "no $zip.sha256 in release v$v" }
    continue  # releases before 0.1.2 have no Windows ARM64 build
  }
  $arch[$a.key] = @{ url = "$base/$zip"; hash = $sha }
  $autoArch[$a.key] = @{ url = "$auto/$zip" }
}

$manifest = [ordered]@{
  version      = $v
  description  = "A command-line audio workstation for scripts and AI agents"
  homepage     = "https://github.com/djynnius/ferraus"
  license      = "Apache-2.0 OR MIT"
  architecture = $arch
  bin          = "ferraus.exe"
  # ONNX Runtime links against the dynamic MSVC runtime
  suggest      = @{ "Visual C++ Redistributable" = "extras/vcredist2022" }
  checkver     = "github"
  autoupdate   = @{ architecture = $autoArch }
}
$json = $manifest | ConvertTo-Json -Depth 6
if ($DryRun) {
  $json
  return
}
git clone "https://x-access-token:$env:GH_TOKEN@github.com/djynnius/scoop-bucket.git" bucket
New-Item -ItemType Directory -Force bucket/bucket | Out-Null
$json | Set-Content bucket/bucket/ferraus.json
Push-Location bucket
git config user.name "ferraus release bot"
git config user.email "release-bot@users.noreply.github.com"
git add -A
git commit -m "ferraus $v"
# an empty bucket repository has no branch yet
git push origin HEAD:main
Pop-Location
