# Licenses in this container image

All the files named here are in /usr/share/doc/ferraus/.

- **ferraus** and **ferraus-plughost**: Apache-2.0 OR MIT (`LICENSE-APACHE`, `LICENSE-MIT`,
  `NOTICE`). The third-party code compiled into them, with full license texts:
  `THIRD-PARTY-LICENSES.md` (also printed by `ferraus licenses --full`).
- **Operating-system packages**, unmodified from Debian 13 "trixie" (the CUDA image: Ubuntu
  24.04). They include FFmpeg, which Debian builds under the GNU General Public License version 2
  or later, and LAME (libmp3lame) under the GNU Lesser General Public License version 2 or later.
  Each package's license is in `/usr/share/doc/<package>/copyright`.
- **Source code** for those packages: `image-packages.tsv` lists every installed package with its
  exact version and source package. The complete corresponding source is in the distribution's
  archive: Debian at `https://snapshot.debian.org/package/<source>/<source-version>/` (or
  `apt-get source <package>=<version>`), Ubuntu at
  `https://launchpad.net/ubuntu/+source/<source>/<source-version>`.
