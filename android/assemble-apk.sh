#!/usr/bin/env bash
#
# Assemble a minimal debug APK around the Android core binary.
#
# Hand-rolled instead of Gradle on purpose: this needs a manifest, one dex and
# two .so files. Pulling in AGP would add an AGP/JDK/SDK version-compat surface
# for no benefit, and build-tools is already on the runner image.
#
# The lib naming below is a platform requirement, not style. Since Android 10
# (API 29) W^X is enforced on the app data directory, so a binary cannot be
# exec'd from filesDir. The native library directory is the one place the
# loader will execute from, and the package manager only extracts entries named
# lib*.so. Hence libopencrabs.so.
#
# Usage: android/assemble-apk.sh <path-to-core-binary>
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-}"
if [ -z "$BIN" ] || [ ! -f "$BIN" ]; then
  echo "usage: $0 <path-to-core-binary>" >&2
  echo "got: '${BIN:-<empty>}'" >&2
  exit 2
fi

SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
if [ -z "$SDK" ] || [ ! -d "$SDK" ]; then
  echo "ANDROID_HOME/ANDROID_SDK_ROOT is not set to a directory (got '${SDK:-<empty>}')" >&2
  exit 2
fi

pick_latest() {
  # sort -V then take the last line: handles android-34 vs android-35 and
  # build-tools 34.0.0 vs 35.0.0 without hardcoding a version that the runner
  # image might not ship.
  ls -d "$1" 2>/dev/null | sort -V | tail -1
}

BT="$(pick_latest "$SDK/build-tools/"*)"
PLATFORM="$(pick_latest "$SDK/platforms/android-"*)"
AJAR="$PLATFORM/android.jar"
for p in "$BT" "$AJAR"; do
  if [ ! -e "$p" ]; then
    echo "missing required SDK component: $p" >&2
    exit 2
  fi
done
echo "build-tools : $BT"
echo "android.jar : $AJAR"
echo "core binary : $BIN ($(stat -c%s "$BIN") bytes)"

OUT="$ROOT/android/out"
rm -rf "$OUT"
mkdir -p "$OUT/classes" "$OUT/stage/lib/arm64-v8a"

# --- 1. native libraries -----------------------------------------------------
cp "$BIN" "$OUT/stage/lib/arm64-v8a/libopencrabs.so"
chmod 755 "$OUT/stage/lib/arm64-v8a/libopencrabs.so"

NDK="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}"
CXX=""
if [ -n "$NDK" ] && [ -d "$NDK" ]; then
  CXX="$(find "$NDK" -name libc++_shared.so -path '*aarch64-linux-android*' 2>/dev/null | head -1)"
fi
if [ -z "$CXX" ]; then
  echo "libc++_shared.so not found under NDK ('${NDK:-<unset>}')" >&2
  echo "the core binary NEEDs it (readelf -d shows it in NEEDED), so the APK" >&2
  echo "would crash at spawn time without it" >&2
  exit 2
fi
cp "$CXX" "$OUT/stage/lib/arm64-v8a/libc++_shared.so"
echo "c++ runtime : $CXX"

# --- 2. java -> dex ----------------------------------------------------------
find "$ROOT/android/java" -name '*.java' > "$OUT/sources.txt"
echo "java sources: $(wc -l < "$OUT/sources.txt")"
javac -source 11 -target 11 -nowarn -classpath "$AJAR" \
  -d "$OUT/classes" "@$OUT/sources.txt" 2>&1 | grep -v 'bootstrap class path' || true

find "$OUT/classes" -name '*.class' > "$OUT/classes.txt"
"$BT/d8" --lib "$AJAR" --min-api 24 --output "$OUT" "@$OUT/classes.txt"
if [ ! -f "$OUT/classes.dex" ]; then
  echo "d8 produced no classes.dex" >&2
  exit 1
fi
echo "dex         : $(stat -c%s "$OUT/classes.dex") bytes"

# --- 3. manifest -> base apk -------------------------------------------------
"$BT/aapt2" link \
  -o "$OUT/base.apk" \
  --manifest "$ROOT/android/AndroidManifest.xml" \
  -I "$AJAR" \
  --min-sdk-version 24 \
  --target-sdk-version 35 \
  --version-code 1 \
  --version-name 0.1.0

# --- 4. fold dex + libs into the apk ----------------------------------------
cp "$OUT/base.apk" "$OUT/unsigned.apk"
cp "$OUT/classes.dex" "$OUT/stage/classes.dex"
(
  cd "$OUT/stage"
  zip -q -X "$OUT/unsigned.apk" classes.dex \
    lib/arm64-v8a/libopencrabs.so \
    lib/arm64-v8a/libc++_shared.so
)

# --- 5. align, sign ----------------------------------------------------------
"$BT/zipalign" -f -p 4 "$OUT/unsigned.apk" "$OUT/aligned.apk"

keytool -genkeypair -v \
  -keystore "$OUT/debug.keystore" \
  -alias androiddebugkey \
  -storepass android -keypass android \
  -dname "CN=Android Debug,O=Android,C=US" \
  -keyalg RSA -keysize 2048 -validity 10000 >/dev/null 2>&1

"$BT/apksigner" sign \
  --ks "$OUT/debug.keystore" \
  --ks-pass pass:android \
  --key-pass pass:android \
  --out "$OUT/opencrabs-debug.apk" \
  "$OUT/aligned.apk"

"$BT/apksigner" verify --print-certs "$OUT/opencrabs-debug.apk" | head -3

# --- 6. verify what we actually built ---------------------------------------
echo
echo "=== badging ==="
"$BT/aapt2" dump badging "$OUT/opencrabs-debug.apk" 2>/dev/null \
  | grep -E "^(package|sdkVersion|targetSdkVersion|application-label|launchable-activity|native-code)" || true
echo
echo "=== zip contents (libs must be present, named lib*.so) ==="
unzip -l "$OUT/opencrabs-debug.apk" | grep -E "classes.dex|lib/" || true
echo
echo "apk: $OUT/opencrabs-debug.apk ($(stat -c%s "$OUT/opencrabs-debug.apk") bytes)"
