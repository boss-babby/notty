#!/usr/bin/env bash
# Two-phone test rig (COLLAB.md 16.3): drives two real devices over adb + UI Automator and checks that a page typed on the host shows up
# on the guest — through the foreground service, NSD discovery and the hotspot hand-off.
#   tools/phone-rig.sh [--dry-run] HOST_SERIAL GUEST_SERIAL [apk]
# Without devices `--dry-run` only checks the tools and the APK (that is what CI does). Output: one PASS/FAIL line per step, exit code = failures.
set -uo pipefail
DRY=0; [ "${1:-}" = "--dry-run" ] && { DRY=1; shift; }
HOST="${1:-}"; GUEST="${2:-}"; APK="${3:-public/downloads/Lumen-android-arm64.apk}"
ADB="${ADB:-$(command -v adb || echo "${ANDROID_HOME:-$HOME/android-sdk}/platform-tools/adb")}"; PKG=com.lumen.app; FAILS=0
step() { printf '%-58s' "$1"; shift; if "$@" >/tmp/rig.out 2>&1; then echo PASS; else echo "FAIL ($(tail -n1 /tmp/rig.out | cut -c1-60))"; FAILS=$((FAILS+1)); fi; }
a() { local s="$1"; shift; "$ADB" -s "$s" "$@"; }
# tap the first UI element whose text / content-desc contains $2 (uiautomator dump -> bounds -> input tap)
tap_text() { local s="$1" t="$2" xml c; xml=$(a "$s" exec-out sh -c 'uiautomator dump /dev/tty >/dev/null 2>&1; cat /sdcard/window_dump.xml 2>/dev/null || uiautomator dump --compressed /dev/stdout') || return 1
  c=$(printf '%s' "$xml" | python3 -c 'import re,sys; x=sys.stdin.read(); t=sys.argv[1].lower(); m=[b for n,b in re.findall(r"(?:text|content-desc)=\"([^\"]*)\"[^>]*bounds=\"\[(\d+),(\d+)\]\[(\d+),(\d+)\]\"",x) if False]
for mm in re.finditer(r"(?:text|content-desc)=\"([^\"]*)\"[^>]*?bounds=\"\[(\d+),(\d+)\]\[(\d+),(\d+)\]\"",x):
    if t in mm.group(1).lower(): a=list(map(int,mm.groups()[1:])); print((a[0]+a[2])//2,(a[1]+a[3])//2); break' "$t")
  [ -n "$c" ] && a "$s" shell input tap $c; }
logcat_has() { a "$1" logcat -d -s Lumen:I | grep -q "$2"; }
command -v "$ADB" >/dev/null || { echo "adb not found (set ADB=...)"; exit 2; }
step "APK present and signed"        bash -c "[ -f '$APK' ] && ${ANDROID_HOME:-$HOME/android-sdk}/build-tools/34.0.0/apksigner verify '$APK'"
[ "$DRY" = 1 ] && { echo "dry run: $FAILS failure(s)"; exit "$FAILS"; }
[ -n "$HOST" ] && [ -n "$GUEST" ] || { echo "usage: $0 [--dry-run] HOST_SERIAL GUEST_SERIAL [apk]"; exit 2; }
for d in "$HOST" "$GUEST"; do
  step "[$d] install"                  a "$d" install -r -g "$APK"
  step "[$d] grant Bluetooth + nearby" bash -c "for p in BLUETOOTH_SCAN BLUETOOTH_ADVERTISE BLUETOOTH_CONNECT NEARBY_WIFI_DEVICES POST_NOTIFICATIONS; do $ADB -s $d shell pm grant $PKG android.permission.\$p 2>/dev/null; done; true"
  step "[$d] launch"                   a "$d" shell am start -n $PKG/.LumenActivity
done
sleep 4
step "host: open People -> Share"      tap_text "$HOST" "Share"
step "host: foreground service running" bash -c "$ADB -s $HOST shell dumpsys activity services $PKG | grep -q CollabService"
step "guest: open Nearby"              tap_text "$GUEST" "Nearby"
sleep 6
step "guest: host found (NSD / beacon)" logcat_has "$GUEST" "found"
step "guest: screen kept on during sync" bash -c "$ADB -s $GUEST shell dumpsys power | grep -qi 'mWakefulness=Awake'"
echo "$FAILS failure(s)"; exit "$FAILS"
