#!/usr/bin/env bash
# Add .deb/.rpm packages to the apt and dnf repositories in a checkout of djynnius/packages and
# sign their metadata with the default key in the current GnuPG keyring.
# Usage: update.sh <repository checkout> <directory with .deb/.rpm files> <public base URL>
set -euo pipefail
repo=$1 pkgs=$2 base=${3%/}

gpg --armor --export > "$repo/ferraus-archive-keyring.asc"

# apt: a flat repository (deb [signed-by=...] <base>/apt ./)
mkdir -p "$repo/apt/pool"
cp "$pkgs"/*.deb "$repo/apt/pool/"
(
  cd "$repo/apt"
  dpkg-scanpackages --multiversion pool > Packages
  gzip -kf Packages
  apt-ftparchive -o APT::FTPArchive::Release::Origin=ferraus -o APT::FTPArchive::Release::Label=ferraus \
    release . > ../Release.tmp
  mv ../Release.tmp Release
  gpg --batch --yes --clearsign -o InRelease Release
  gpg --batch --yes -abs -o Release.gpg Release
)
cat > "$repo/apt/ferraus.sources" <<SOURCES
Types: deb
URIs: $base/apt
Suites: ./
Signed-By: /etc/apt/keyrings/ferraus-archive-keyring.asc
SOURCES

# dnf: packages are not signed individually; the repository metadata is
mkdir -p "$repo/rpm"
cp "$pkgs"/*.rpm "$repo/rpm/"
createrepo_c --update "$repo/rpm"
gpg --batch --yes --detach-sign --armor "$repo/rpm/repodata/repomd.xml"
cat > "$repo/rpm/ferraus.repo" <<REPO
[ferraus]
name=ferraus
baseurl=$base/rpm
enabled=1
gpgcheck=0
repo_gpgcheck=1
gpgkey=$base/ferraus-archive-keyring.asc
REPO

touch "$repo/.nojekyll"
