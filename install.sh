#!/usr/bin/env sh
set -eu

repo="${FONTBREW_REPO:-yyyanghj/fontbrew}"
version="${FONTBREW_VERSION:-latest}"
install_dir="${FONTBREW_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

need_command() {
  command -v "$1" >/dev/null 2>&1 || fail "fontbrew installer requires $1."
}

os="$(uname -s)"
arch="$(uname -m)"

[ "$os" = "Darwin" ] || fail "fontbrew currently supports macOS only."

case "$arch" in
  arm64 | aarch64)
    target="aarch64-apple-darwin"
    ;;
  x86_64 | amd64)
    target="x86_64-apple-darwin"
    ;;
  *)
    fail "unsupported macOS architecture: $arch"
    ;;
esac

need_command curl
need_command tar
need_command shasum

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT INT TERM

asset="fontbrew-${target}.tar.gz"

if [ "$version" = "latest" ]; then
  download_base="https://github.com/${repo}/releases/latest/download"
else
  download_base="https://github.com/${repo}/releases/download/${version}"
fi

archive_path="${tmp_dir}/${asset}"
checksum_path="${archive_path}.sha256"

curl -fsSL "${download_base}/${asset}" -o "$archive_path"
curl -fsSL "${download_base}/${asset}.sha256" -o "$checksum_path"

(
  cd "$tmp_dir"
  shasum -a 256 -c "${asset}.sha256"
)

tar -xzf "$archive_path" -C "$tmp_dir"

mkdir -p "$install_dir"
install -m 0755 "${tmp_dir}/fontbrew-${target}/fontbrew" "${install_dir}/fontbrew"

printf 'fontbrew installed to %s/fontbrew\n' "$install_dir"

case ":$PATH:" in
  *":$install_dir:"*) ;;
  *)
    printf 'Add %s to PATH before running fontbrew.\n' "$install_dir" >&2
    ;;
esac
