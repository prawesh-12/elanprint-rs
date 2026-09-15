#!/bin/sh
# Build the .deb into target/deb/.
#
#   --no-build   package target/release as it stands
set -e
cd "$(dirname "$0")/.."

BUILD=1
[ "$1" = "--no-build" ] && BUILD=0

PKG=elanprint-rs
ARCH=$(dpkg --print-architecture)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
MAINTAINER="Prawesh Mandal <nirazanmandal60@gmail.com>"
[ -n "$VERSION" ] || { echo "no version in Cargo.toml"; exit 1; }
[ "$ARCH" = "amd64" ] || { echo "only amd64 is built and tested"; exit 1; }

if [ "$BUILD" = "1" ]; then
    JOBS=$(( $(nproc) - 2 ))
    [ "$JOBS" -lt 1 ] && JOBS=1
    cargo build --release --workspace -j "$JOBS"
fi

for b in elanprintd elanprint-cli elanprint-login; do
    [ -f "target/release/$b" ] || { echo "missing target/release/$b"; exit 1; }
done

ROOT=target/deb/$PKG
rm -rf target/deb
mkdir -p "$ROOT/DEBIAN"

install -D -m 0755 target/release/elanprintd    "$ROOT/usr/libexec/elanprintd"
install -D -m 0755 target/release/elanprint-cli "$ROOT/usr/libexec/elanprint-cli"
# X11 takes WM_CLASS from the binary name, which must match StartupWMClass
install -D -m 0755 target/release/elanprint-login "$ROOT/usr/bin/elanprint-rs"

install -D -m 0644 systemd/elanprintd.service \
    "$ROOT/usr/lib/systemd/system/elanprintd.service"
install -D -m 0644 udev/70-elanprint.rules \
    "$ROOT/usr/lib/udev/rules.d/70-elanprint.rules"
install -D -m 0644 desktop/elanprint-rs.desktop \
    "$ROOT/usr/share/applications/elanprint-rs.desktop"
for s in 48 64 128 256 512; do
    install -D -m 0644 "assets/icon-$s.png" \
        "$ROOT/usr/share/icons/hicolor/${s}x${s}/apps/elanprint-rs.png"
done
install -D -m 0644 LICENSE "$ROOT/usr/share/doc/$PKG/copyright"
install -D -m 0644 README.md "$ROOT/usr/share/doc/$PKG/README.md"
gzip -9n "$ROOT/usr/share/doc/$PKG/README.md"

# dpkg-shlibdeps reads a debian/control next to where it runs
WORK=target/deb/shlibdeps
mkdir -p "$WORK/debian"
printf 'Source: %s\n\nPackage: %s\nArchitecture: %s\n' "$PKG" "$PKG" "$ARCH" \
    > "$WORK/debian/control"
SHLIBS=$(cd "$WORK" && dpkg-shlibdeps -O --ignore-missing-info \
    ../../../"$ROOT"/usr/libexec/elanprintd \
    ../../../"$ROOT"/usr/libexec/elanprint-cli \
    ../../../"$ROOT"/usr/bin/elanprint-rs 2>/dev/null \
    | sed -n 's/^shlibs:Depends=//p')
if [ -z "$SHLIBS" ]; then
    echo "warning: dpkg-shlibdeps found nothing, falling back to a guess." >&2
    echo "         Install dpkg-dev and rebuild before publishing this." >&2
    SHLIBS="libc6 (>= 2.35)"
fi
rm -rf "$WORK"

# eframe dlopens these, so dpkg-shlibdeps cannot see them in the ELF header
GUI="libx11-6, libx11-xcb1, libxcb1, libxkbcommon0, libgl1, libegl1, libwayland-client0"

SIZE=$(du -ks "$ROOT" | cut -f1)

cat > "$ROOT/DEBIAN/control" <<EOF
Package: $PKG
Version: $VERSION
Architecture: $ARCH
Maintainer: $MAINTAINER
Installed-Size: $SIZE
Depends: $SHLIBS, $GUI, libpam-fprintd, libc-bin, systemd, udev
Section: utils
Priority: optional
Description: userspace driver for the ELAN 04f3:0c90 fingerprint sensor
 Linux support for the ELAN 04f3:0c90 fingerprint sensor, which libfprint
 does not cover. Pure Rust, userspace, no kernel module.
 .
 Enrolment and matching run on the sensor, so no fingerprint image reaches
 the host. The daemon owns net.reactivated.Fprint on the system bus, which
 is what makes pam_fprintd, the lock screen and GDM greeter login work.
 .
 This package masks fprintd.service, which owns the same bus name and has
 no driver for this sensor. Removing the package unmasks it again.
EOF

cp packaging/deb/postinst packaging/deb/prerm packaging/deb/postrm "$ROOT/DEBIAN/"
chmod 0755 "$ROOT/DEBIAN/postinst" "$ROOT/DEBIAN/prerm" "$ROOT/DEBIAN/postrm"

(cd "$ROOT" && find usr -type f -print0 \
    | xargs -0 md5sum > DEBIAN/md5sums)

OUT="target/deb/${PKG}_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group --build "$ROOT" "$OUT" >/dev/null
rm -rf "$ROOT"

sha256sum "$OUT" | sed "s|target/deb/||" > "target/deb/sha256sums.txt"
echo "built $OUT"
