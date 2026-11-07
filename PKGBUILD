# Maintainer: timon <timon2201@gmail.com>
#
# Part of the aion-cube-overlay repository: builds the working copy this file sits in, as it is on disk
# (uncommitted changes included), without cloning or downloading the sources. From the repository root:
#   makepkg -si

pkgname=aion-cube-overlay
# The version comes from Cargo.toml plus the commit, worked out here instead of in a pkgver() function, because
# makepkg writes a pkgver() result back into this file, which would leave it modified in git after every build.
_version() {
  local version
  version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$startdir/Cargo.toml" | head -n1)
  if git -C "$startdir" rev-parse --git-dir >/dev/null 2>&1; then
    printf '%s.r%s.g%s' "$version" "$(git -C "$startdir" rev-list --count HEAD)" "$(git -C "$startdir" rev-parse --short=7 HEAD)"
  else
    printf '%s' "$version"
  fi
}
pkgver=$(_version)
pkgrel=1
pkgdesc='Aion 2 Hidden Cube radar (with height) and per-enemy DPS meter as Wayland overlays'
arch=('x86_64' 'aarch64')
license=('GPL-3.0-or-later')
depends=('libpcap' 'libcap' 'gcc-libs' 'glibc')
makedepends=('cargo')
optdepends=(
  'fontconfig: find a system font for the text (fc-match)'
  'noto-fonts: a font for the text, if none is installed'
  'pipewire: sounds with --alert-cmd "pw-play ..."'
  'libnotify: notifications with --alert-cmd "notify-send ..."'
)
install="$pkgname.install"

# The build output goes to src/target (kept between runs, so rebuilds are quick), not to the repository's own
# target/ directory.
_target() {
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_TARGET_DIR="$srcdir/target"
}

prepare() {
  cd "$startdir"
  _target
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd "$startdir"
  _target
  cargo build --frozen --release
}

check() {
  cd "$startdir"
  _target
  cargo test --frozen --workspace
}

package() {
  cd "$startdir"
  install -Dm755 "$srcdir/target/release/$pkgname" "$pkgdir/usr/bin/$pkgname"
  install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"
  install -Dm644 docs/preview.png "$pkgdir/usr/share/doc/$pkgname/preview.png"
  install -Dm644 docs/meter.png "$pkgdir/usr/share/doc/$pkgname/meter.png"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
