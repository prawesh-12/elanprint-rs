#!/bin/sh
# Touches no file in /etc/pam.d. GDM's gdm-fingerprint stack does not
# include common-auth, so greeter login needs no PAM edit.
# Undo with tools/uninstall.sh.
set -e
cd "$(dirname "$0")/.."

VID=04f3
PID=0c90

[ "$(id -u)" = "0" ] || { echo "run with sudo"; exit 1; }
for b in elanprintd elanprint-cli elanprint-login; do
    [ -f "target/release/$b" ] || {
        echo "build first: cargo build --release --workspace"
        exit 1
    }
done

fail() { echo "error: $*" >&2; exit 1; }
warn() { echo "warning: $*" >&2; }

echo "checking this machine"

. "$(dirname "$0")/usb-locate.sh"
if elanprint_locate >/dev/null 2>&1; then
    echo "  sensor $VID:$PID on bus $ELANPRINT_BUS device $ELANPRINT_DEV"
else
    for d in /sys/bus/usb/devices/*; do
        [ -r "$d/idVendor" ] || continue
        [ "$(cat "$d/idVendor")" = "$VID" ] || continue
        fail "found ELAN $VID:$(cat "$d/idProduct"), but only $VID:$PID is supported"
    done
    fail "no $VID:$PID fingerprint sensor found. Check with: lsusb -d $VID:$PID"
fi

if [ -r /etc/os-release ]; then
    . /etc/os-release
    case "$ID" in
        ubuntu)
            MAJOR=${VERSION_ID%%.*}
            if [ "${MAJOR:-0}" -lt 24 ] 2>/dev/null; then
                warn "Ubuntu $VERSION_ID is below 24.04, this is untested there"
            else
                echo "  $PRETTY_NAME"
            fi
            ;;
        *) warn "$PRETTY_NAME is not Ubuntu, only Ubuntu 24.04+ has been run" ;;
    esac
else
    warn "no /etc/os-release, cannot check the distribution"
fi

command -v systemctl >/dev/null 2>&1 || fail "no systemctl, this needs systemd"
[ -f /etc/pam.d/gdm-fingerprint ] \
    && echo "  /etc/pam.d/gdm-fingerprint present, greeter login should work" \
    || warn "no /etc/pam.d/gdm-fingerprint. Enrol and verify will work, greeter
           login will not until a PAM stack calls pam_fprintd. Install
           libpam-fprintd and a display manager that ships that stack"
[ -f /usr/lib/x86_64-linux-gnu/security/pam_fprintd.so ] \
    || [ -f /lib/x86_64-linux-gnu/security/pam_fprintd.so ] \
    || warn "pam_fprintd.so not found, install libpam-fprintd for login"

echo "installing"

echo "  binary -> /usr/libexec/elanprintd"
install -D -m 0755 target/release/elanprintd /usr/libexec/elanprintd

# --wipe-device needs this, so an installed-only checkout can still erase
echo "  cli    -> /usr/libexec/elanprint-cli"
install -D -m 0755 target/release/elanprint-cli /usr/libexec/elanprint-cli

# on X11 the window manager takes WM_CLASS from the binary name, so the
# installed name has to match StartupWMClass in the desktop entry
echo "  app    -> /usr/bin/elanprint-rs"
install -D -m 0755 target/release/elanprint-login /usr/bin/elanprint-rs

echo "  icons  -> /usr/share/icons/hicolor"
for s in 48 64 128 256; do
    install -D -m 0644 "assets/icon-$s.png" \
        "/usr/share/icons/hicolor/${s}x${s}/apps/elanprint-rs.png"
done
install -D -m 0644 desktop/elanprint-rs.desktop \
    /usr/share/applications/elanprint-rs.desktop
command -v gtk-update-icon-cache >/dev/null 2>&1 \
    && gtk-update-icon-cache -qf /usr/share/icons/hicolor 2>/dev/null || true
command -v update-desktop-database >/dev/null 2>&1 \
    && update-desktop-database -q /usr/share/applications 2>/dev/null || true

# The store starts empty. Enrolling writes it. Never seeded from anywhere.
echo "  store  -> /var/lib/elanprint/"
install -d -m 0700 /var/lib/elanprint
if [ -f /var/lib/elanprint/prints.json ]; then
    echo "         keeping the existing store"
fi

echo "  udev   -> /etc/udev/rules.d/70-elanprint.rules"
install -m 0644 udev/70-elanprint.rules /etc/udev/rules.d/70-elanprint.rules
udevadm control --reload-rules
udevadm trigger --action=add --subsystem-match=usb --attr-match=idVendor=$VID

echo "  unit   -> /etc/systemd/system/elanprintd.service"
install -m 0644 systemd/elanprintd.service /etc/systemd/system/elanprintd.service
systemctl daemon-reload

# The stock fprintd owns net.reactivated.Fprint and has no 0c90 driver, so it
# would answer the greeter with no device. Masked so D-Bus activation cannot
# start it behind this daemon's back. Unmasked by the uninstall script.
if systemctl list-unit-files fprintd.service >/dev/null 2>&1; then
    echo "  masking fprintd.service"
    systemctl mask fprintd.service
fi

# restart, not enable --now: on a reinstall the unit is already active and
# --now would leave the old binary running.
echo "  starting elanprintd"
systemctl enable elanprintd.service
systemctl restart elanprintd.service
sleep 2

# verify what is on the machine, not what the steps above intended
echo "verifying"
VERIFY_FAILED=0
check() {
    if [ "$1" = "0" ]; then
        echo "  ok    $2"
    else
        echo "  FAIL  $2" >&2
        VERIFY_FAILED=1
    fi
}

[ -x /usr/libexec/elanprintd ]; check $? "/usr/libexec/elanprintd is present and executable"
[ -x /usr/libexec/elanprint-cli ]; check $? "/usr/libexec/elanprint-cli is present and executable"
[ -x /usr/bin/elanprint-rs ]; check $? "/usr/bin/elanprint-rs is present and executable"
[ -f /usr/share/applications/elanprint-rs.desktop ]; check $? "desktop entry is installed"
[ -f /usr/share/icons/hicolor/256x256/apps/elanprint-rs.png ]; check $? "icon is installed"
[ -f /etc/systemd/system/elanprintd.service ]; check $? "unit file is installed"
[ -d /var/lib/elanprint ]; check $? "store directory exists"

# the running binary must be the one just built, not a leftover
BUILT=$(sha256sum target/release/elanprintd | cut -d" " -f1)
LIVE=$(sha256sum /usr/libexec/elanprintd 2>/dev/null | cut -d" " -f1)
[ -n "$LIVE" ] && [ "$BUILT" = "$LIVE" ]
check $? "installed binary matches the one just built"

systemctl is-active --quiet elanprintd.service
check $? "elanprintd.service is active"

# owning the bus name is the whole integration, so prove it
busctl --system status net.reactivated.Fprint >/dev/null 2>&1
check $? "net.reactivated.Fprint is owned"

OWNER=$(busctl --system status net.reactivated.Fprint 2>/dev/null | sed -n "s/^PID=//p")
MAIN=$(systemctl show elanprintd.service -p MainPID --value 2>/dev/null)
[ -n "$OWNER" ] && [ "$OWNER" = "$MAIN" ]
check $? "the bus name is owned by elanprintd (pid ${MAIN:-none}), not a stale registration"

if [ "$VERIFY_FAILED" != "0" ]; then
    echo >&2
    echo "install did NOT complete. Nothing above should be trusted." >&2
    systemctl --no-pager --lines=20 status elanprintd.service >&2 2>&1 || true
    exit 1
fi

echo
echo "installed and verified."
if [ -s /var/lib/elanprint/prints.json ]; then
    echo "store: $(tr -d "\n " < /var/lib/elanprint/prints.json)"
else
    echo "store: empty. Enrol a finger in GNOME Settings, or with any fprintd client."
    echo "       If you are upgrading and templates are already in the sensor, the"
    echo "       name to slot mapping must be carried across, not re-enrolled."
fi
