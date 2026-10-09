#!/bin/sh
# Installs agentcam from GitHub releases.
#   curl -fsSL https://raw.githubusercontent.com/davekiss/agentcam/main/install.sh | sh
# AGENTCAM_VERSION=v0.1.0 pins a release; AGENTCAM_INSTALL_DIR picks where it goes (default ~/.local/bin).
set -eu

repo=davekiss/agentcam
dir=${AGENTCAM_INSTALL_DIR:-$HOME/.local/bin}
version=${AGENTCAM_VERSION:-latest}

fail() {
  echo "agentcam install: $*" >&2
  exit 1
}

case "$(uname -s)" in
  Linux) os=unknown-linux-musl ;;
  Darwin) os=apple-darwin ;;
  *) fail "no build for $(uname -s); build from source: https://github.com/$repo#build-from-source" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) fail "no build for $(uname -m); build from source: https://github.com/$repo#build-from-source" ;;
esac

asset=agentcam-$arch-$os.tar.gz
if [ "$version" = latest ]; then
  base=https://github.com/$repo/releases/latest/download
else
  base=https://github.com/$repo/releases/download/$version
fi

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q "$1" -O "$2"; }
else
  fail "needs curl or wget"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fetch "$base/$asset" "$tmp/$asset" || fail "could not download $base/$asset"
fetch "$base/$asset.sha256" "$tmp/$asset.sha256" || fail "could not download the checksum for $asset"

want=$(cut -d' ' -f1 "$tmp/$asset.sha256")
if command -v sha256sum >/dev/null 2>&1; then
  got=$(sha256sum "$tmp/$asset" | cut -d' ' -f1)
else
  got=$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)
fi
[ "$want" = "$got" ] || fail "checksum mismatch for $asset"

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$dir"
install -m 755 "$tmp/agentcam-$arch-$os/agentcam" "$dir/agentcam"

echo "installed $("$dir/agentcam" --version) to $dir/agentcam" >&2
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "add $dir to your PATH: export PATH=\"$dir:\$PATH\"" >&2 ;;
esac
command -v ffmpeg >/dev/null 2>&1 || echo "export needs ffmpeg, and the x11 source needs Xvfb (apt-get install -y ffmpeg xvfb)" >&2
echo "run 'agentcam doctor' to see what this machine can record" >&2
