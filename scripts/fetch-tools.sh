#!/usr/bin/env bash
# Fetches the ffprobe and the TSDuck that the conformance tests compare this
# crate's reading with, each checked against the hash pinned here, and puts
# them in <directory>/bin.
#
#   scripts/fetch-tools.sh target/tools
#   PATH="$PWD/target/tools/bin:$PATH" cargo test --workspace --all-features -- --ignored
#
# TSDuck needs at run time the packages its own package depends on, which are
# not fetched: <directory>/tsduck-depends names them for the host's apt-get.
set -euo pipefail

# FFmpeg-Builds keeps each month's last build, so this one stays downloadable.
ffmpeg_url=https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/ffmpeg-n8.1.3-9-g29e619e767-linux64-lgpl-8.1.tar.xz
ffmpeg_sha256=dfa863a00ca81f1bdf58a372b18cff4820f0017e55de32778de8ecd8ed92a02e
tsduck_url=https://github.com/tsduck/tsduck/releases/download/v3.45-4798/tsduck_3.45-4798.ubuntu24_amd64.deb
tsduck_sha256=0023689f76e75b64e45253771208bcf8ca95b91fbfd0900da097c729f3c39fde
destination=${1:?usage: $0 <directory>}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fetch() {
  local url=$1
  local sha256=$2
  local file
  file="$work/$(basename "$url")"
  curl -sSfL --retry 3 -o "$file" "$url"
  echo "$sha256  $file" | sha256sum -c -
}

fetch "$ffmpeg_url" "$ffmpeg_sha256"
fetch "$tsduck_url" "$tsduck_sha256"

rm -rf "$destination"
mkdir -p "$destination/bin"
# The wrappers below name the tree by absolute path, so they run from anywhere.
destination=$(cd "$destination" && pwd)
tar -xJf "$work/$(basename "$ffmpeg_url")" -C "$destination/bin" --strip-components=2 --wildcards '*/bin/ffprobe'

# The package is unpacked rather than installed, so no root is needed. Its
# tree is kept whole: TSDuck finds its configuration in ../share/tsduck from
# the directory of its executable.
dpkg-deb -x "$work/$(basename "$tsduck_url")" "$destination/tsduck"
dpkg-deb -f "$work/$(basename "$tsduck_url")" Depends \
  | tr ',' '\n' | sed -E 's/\(.*\)//; s/\|.*//; s/[[:space:]]//g; /^$/d' > "$destination/tsduck-depends"
libraries=$(dirname "$(find "$destination/tsduck" -name 'libtsduck.so*' -print -quit)")
for tool in tstables tsanalyze; do
  cat > "$destination/bin/$tool" <<EOF
#!/bin/sh
LD_LIBRARY_PATH="$libraries\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}" exec "$destination/tsduck/usr/bin/$tool" "\$@"
EOF
  chmod +x "$destination/bin/$tool"
done
