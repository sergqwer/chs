#!/bin/sh
# chs installer for Linux and macOS:
#
#   curl -fsSL https://raw.githubusercontent.com/sergqwer/chs/main/install.sh | sh
#
# Puts the latest release's binary in ~/.local/bin and runs it once, which links it
# into /usr/local/bin (with `sudo -n` when that is allowed) and writes the default
# config. Run it again to update. CHS_VERSION=v0.2.1 pins a release,
# CHS_INSTALL_DIR=/some/dir changes where the binary goes.
set -eu

repo=sergqwer/chs
case "$(uname -s)/$(uname -m)" in
Linux/x86_64 | Linux/amd64) asset=chs-linux-x86_64.tar.gz ;;
Linux/aarch64 | Linux/arm64) asset=chs-linux-aarch64.tar.gz ;;
Darwin/arm64) asset=chs-macos-aarch64.tar.gz ;;
*)
	echo "chs: no prebuilt binary for $(uname -s) $(uname -m); build it with cargo, see the README" >&2
	exit 1
	;;
esac
if [ -n "${CHS_VERSION:-}" ]; then
	url="https://github.com/$repo/releases/download/$CHS_VERSION/$asset"
else
	url="https://github.com/$repo/releases/latest/download/$asset"
fi
dir="${CHS_INSTALL_DIR:-$HOME/.local/bin}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
echo "Downloading $url"
if command -v curl >/dev/null 2>&1; then
	curl -fsSL "$url" -o "$tmp/chs.tar.gz"
elif command -v wget >/dev/null 2>&1; then
	wget -qO "$tmp/chs.tar.gz" "$url"
else
	echo "chs: needs curl or wget" >&2
	exit 1
fi
mkdir "$tmp/x"
tar -xzf "$tmp/chs.tar.gz" -C "$tmp/x"

mkdir -p "$dir"
# Renamed into place: a running chs keeps its old copy, and a symlink already
# sitting there is replaced instead of written through.
cp "$tmp/x/chs" "$dir/.chs.new"
chmod 755 "$dir/.chs.new"
mv -f "$dir/.chs.new" "$dir/chs"

# The first run does the integration: PATH link, default config.
"$dir/chs" --list -n 0 >/dev/null
echo "Installed $("$dir/chs" --version) to $dir/chs"
if ! command -v chs >/dev/null 2>&1; then
	echo "Add $dir to PATH (or open a new shell) to run it as chs"
fi
