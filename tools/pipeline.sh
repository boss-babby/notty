#!/usr/bin/env bash
# provision -> check + tests -> WebAssembly -> Windows -> Android. Stops at the first failing stage. Log: /tmp/pipeline.log
cd "$(dirname "$0")/.."
export HOME="${HOME:-/home/user}" CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/lt}" CARGO_BUILD_JOBS=3 CARGO_JOBS=3
stage() { echo "[$(date +%T)] ===== $1"; }
[ -z "${SKIP_TOOLS:-}" ] && { stage provision; bash tools/provision.sh || { echo "STAGE-FAIL provision"; exit 1; }; }
. "$HOME/.cargo/env"
export ANDROID_HOME="$HOME/android-sdk"
if [ -z "${SKIP_CHECK:-}" ]; then
  stage check; cargo check --lib --tests -j3 2>&1 | grep -E "^error" -A12 | head -120
  [ "${PIPESTATUS[0]}" = 0 ] || true
  cargo check --lib --tests -j3 >/dev/null 2>&1 || { echo "STAGE-FAIL check"; exit 1; }
fi
[ -z "${SKIP_TESTS:-}" ] && { stage tests; cargo test --lib -j3 2>&1 | grep -E "^test result|FAILED|failed|panicked" | head -30; }
stage web; bash build-web.sh > /tmp/web.log 2>&1 || { tail -40 /tmp/web.log; echo "STAGE-FAIL web"; exit 1; }
stage windows; bash build-windows.sh > /tmp/win.log 2>&1 || { tail -40 /tmp/win.log; echo "STAGE-FAIL windows"; exit 1; }
stage android; bash build-android.sh > /tmp/and.log 2>&1 || { tail -40 /tmp/and.log; echo "STAGE-FAIL android"; exit 1; }
stage done; ls -la ../public/downloads ../public/webapp | head -30
echo PIPELINE-OK
