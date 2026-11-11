#!/bin/sh
# Builds the .deb and/or .rpm inside containers of those distributions, from any Linux with podman or docker.
#
#   packaging/build-packages.sh          both
#   packaging/build-packages.sh deb      only the .deb
#   packaging/build-packages.sh rpm      only the .rpm
#
# The packages land in dist/. Building in the target distribution matters: there the binary links that
# distribution's libpcap and an older glibc, and cargo-deb can work out the package dependencies. A package
# built on a newer system (Arch, CachyOS) needs a newer glibc than Debian or Ubuntu have and has no libpcap
# dependency.
#
# The images set the oldest release the packages install on. Override them if needed:
#   DEB_IMAGE=docker.io/library/debian:forky RPM_IMAGE=registry.fedoraproject.org/fedora:45 packaging/build-packages.sh
set -eu

DEB_IMAGE=${DEB_IMAGE:-docker.io/library/debian:trixie}
RPM_IMAGE=${RPM_IMAGE:-registry.fedoraproject.org/fedora:44}

cd "$(dirname "$0")/.."
engine=$(command -v podman || command -v docker || true)
[ -n "$engine" ] || { echo "Needs podman or docker." >&2; exit 1; }
mkdir -p dist

# Rootless podman already writes files as the calling user; docker writes them as root, so they are handed over.
case "$engine" in
    *podman) CHOWN=true ;;
    *) CHOWN="chown $(id -u):$(id -g) /dist/*" ;;
esac

# Runs a build script in a container, with the sources read-only at /src and the output directory at /dist.
run() {
    "$engine" run --rm -v "$PWD:/src:ro,z" -v "$PWD/dist:/dist:z" "$1" sh -euc "$2
        $CHOWN"
}

# Copies the sources into the container without the build output (target/, and makepkg's src/ and pkg/), so the
# host's build files are neither used nor touched. Runs after the package installs, which bring tar.
COPY='mkdir /build && tar -C /src --anchored --exclude=./target --exclude=./dist --exclude=./src --exclude=./pkg -cf - . | tar -C /build -xf - && cd /build'

# rustup instead of the distribution's Rust, which can be too old for the dependencies.
RUSTUP='curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal; . "$HOME/.cargo/env"'

deb() {
    echo ">>> Building the .deb in $DEB_IMAGE"
    run "$DEB_IMAGE" "
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends ca-certificates curl build-essential pkg-config libpcap-dev dpkg-dev tar >/dev/null
        $COPY
        $RUSTUP
        cargo install -q --locked cargo-deb
        cargo deb -p cube-overlay --locked
        cp target/debian/*.deb /dist/
    "
}

rpm() {
    echo ">>> Building the .rpm in $RPM_IMAGE"
    run "$RPM_IMAGE" "
        dnf install -y -q gcc pkgconf-pkg-config libpcap-devel curl tar >/dev/null
        $COPY
        $RUSTUP
        cargo install -q --locked cargo-generate-rpm
        cargo build --release --locked
        cargo generate-rpm -p crates/cube-overlay
        cp target/generate-rpm/*.rpm /dist/
    "
}

case "${1:-all}" in
    deb) deb ;;
    rpm) rpm ;;
    all) deb; rpm ;;
    *) echo "Usage: $0 [deb|rpm|all]" >&2; exit 2 ;;
esac

echo ">>> Done:"
ls -1 dist/
