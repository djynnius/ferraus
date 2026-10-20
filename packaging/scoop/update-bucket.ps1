# Update the scoop manifest in djynnius/scoop-bucket for a release tag (run by CI only).
param([Parameter(Mandatory = $true)][string]$Version)
$ErrorActionPreference = "Stop"
$v = $Version.TrimStart("v")
$base = "https://github.com/djynnius/ferraus/releases/download/v$v"
$sha = (Invoke-WebRequest "$base/ferraus-x86_64-pc-windows-msvc.zip.sha256").Content.Split(" ")[0].Trim()
$manifest = [ordered]@{
  version      = $v
  description  = "A command-line audio workstation for scripts and AI agents"
  homepage     = "https://github.com/djynnius/ferraus"
  license      = "Apache-2.0 OR MIT"
  architecture = @{ "64bit" = @{ url = "$base/ferraus-x86_64-pc-windows-msvc.zip"; hash = $sha } }
  bin          = "ferraus.exe"
  # ONNX Runtime links against the dynamic MSVC runtime
  suggest      = @{ "Visual C++ Redistributable" = "extras/vcredist2022" }
  checkver     = "github"
  autoupdate   = @{ architecture = @{ "64bit" = @{ url = "https://github.com/djynnius/ferraus/releases/download/v`$version/ferraus-x86_64-pc-windows-msvc.zip" } } }
}
git clone "https://x-access-token:$env:GH_TOKEN@github.com/djynnius/scoop-bucket.git" bucket
New-Item -ItemType Directory -Force bucket/bucket | Out-Null
$manifest | ConvertTo-Json -Depth 6 | Set-Content bucket/bucket/ferraus.json
Push-Location bucket
git config user.name "ferraus release bot"
git config user.email "release-bot@users.noreply.github.com"
git add -A
git commit -m "ferraus $v"
# an empty bucket repository has no branch yet
git push origin HEAD:main
Pop-Location
