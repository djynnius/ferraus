# Update the scoop manifest in djynnius/scoop-bucket for a release tag (run by CI only).
param([Parameter(Mandatory = $true)][string]$Version)
$ErrorActionPreference = "Stop"
$v = $Version.TrimStart("v")
$base = "https://github.com/djynnius/ferraus/releases/download/v$v"
$sha = (Invoke-WebRequest "$base/ferraus-x86_64-pc-windows-msvc.zip.sha256").Content.Split(" ")[0].Trim()
$manifest = [ordered]@{
  version      = $v
  description  = "Music editing CLI for people and AI agents"
  homepage     = "https://github.com/djynnius/ferraus"
  license      = "Apache-2.0 OR MIT"
  architecture = @{ "64bit" = @{ url = "$base/ferraus-x86_64-pc-windows-msvc.zip"; hash = $sha } }
  bin          = "ferraus.exe"
  checkver     = "github"
  autoupdate   = @{ architecture = @{ "64bit" = @{ url = "https://github.com/djynnius/ferraus/releases/download/v`$version/ferraus-x86_64-pc-windows-msvc.zip" } } }
}
git clone "https://x-access-token:$env:GH_TOKEN@github.com/djynnius/scoop-bucket.git" bucket
$manifest | ConvertTo-Json -Depth 6 | Set-Content bucket/bucket/ferraus.json
Push-Location bucket
git config user.name "ferraus release bot"
git config user.email "release-bot@users.noreply.github.com"
git add -A
git commit -m "ferraus $v"
git push
Pop-Location
