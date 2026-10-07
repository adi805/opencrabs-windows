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
# Usage: android/assemble-apk.sh <abi>=<path-to-core-binary> [...]
#        android/assemble-apk.sh <path-to-core-binary>   (legacy: arm64-v8a)
#
# More than one ABI is allowed so an x86_64 APK can be produced for the
# emulator leg. An arm64-only APK cannot be installed on an x86_64 AVD, and
# leaning on the API 35 ARM-translation layer to run it would be a community
# claim, not a contract this repo can verify.
set -euo pipefail

# minSdk 26 is a product contract, not a build detail: the PRD pins it in
# "Layer 2: Machine Spec" and NFR-005, and AC-026 verifies it on a real
# device. It is also the floor the Java shell actually needs (NotificationChannel,
# plus Process.isAlive/destroyForcibly). Declared once so the dex and the
# manifest cannot drift apart, which is how it read 24 in both places before.
MIN_SDK=26

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [ "$#" -eq 0 ]; then
  echo "usage: $0 <abi>=<path-to-core-binary> [...]" >&2
  echo "       $0 <path-to-core-binary>    (legacy: arm64-v8a)" >&2
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

OUT="$ROOT/android/out"
rm -rf "$OUT"
mkdir -p "$OUT/classes"
: > "$OUT/libs.txt"

NDK="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}"

# --- 1. native libraries -----------------------------------------------------
#
# One lib/<abi>/ directory per ABI. libc++_shared.so is per-ABI inside the NDK:
# the aarch64 copy will not load on x86_64 and vice versa, so the lookup has to
# key off the ABI being staged rather than assume aarch64.
#
# The NDK names its ABI directories after the target TRIPLE, not after the APK
# ABI name: arm64-v8a lives under aarch64-linux-android. Matching on the APK ABI
# name therefore finds nothing for arm64, and a bare `find | head -1` with no
# path filter could hand back another ABI's copy, which the loader refuses at
# spawn time. Map explicitly and fail loudly on an unmapped ABI.
abi_triple() {
  case "$1" in
    arm64-v8a)   echo "aarch64-linux-android" ;;
    armeabi-v7a) echo "arm-linux-androideabi" ;;
    x86)         echo "i686-linux-android" ;;
    x86_64)      echo "x86_64-linux-android" ;;
    *)           echo "" ;;
  esac
}

stage_abi() {
  abi="$1"
  bin="$2"
  if [ ! -f "$bin" ]; then
    echo "core binary not found for $abi: '$bin'" >&2
    exit 2
  fi
  mkdir -p "$OUT/stage/lib/$abi"
  cp "$bin" "$OUT/stage/lib/$abi/libopencrabs.so"
  chmod 755 "$OUT/stage/lib/$abi/libopencrabs.so"
  echo "core binary : $abi <- $bin ($(stat -c%s "$bin") bytes)"

  cxx=""
  triple="$(abi_triple "$abi")"
  if [ -z "$triple" ]; then
    echo "unknown ABI '$abi': no NDK triple mapping, cannot locate libc++_shared.so" >&2
    exit 2
  fi
  if [ -n "$NDK" ] && [ -d "$NDK" ]; then
    cxx="$(find "$NDK" -name libc++_shared.so -path "*${triple}*" 2>/dev/null | head -1)"
  fi
  if [ -z "$cxx" ]; then
    echo "libc++_shared.so not found for $abi (NDK triple '$triple') under NDK ('${NDK:-<unset>}')" >&2
    echo "the core binary NEEDs it (readelf -d shows it in NEEDED), so the APK" >&2
    echo "would crash at spawn time without it" >&2
    exit 2
  fi
  cp "$cxx" "$OUT/stage/lib/$abi/libc++_shared.so"
  echo "c++ runtime : $abi <- $cxx"

  printf '%s\n' "lib/$abi/libopencrabs.so" "lib/$abi/libc++_shared.so" >> "$OUT/libs.txt"
}

for arg in "$@"; do
  case "$arg" in
    *=*) stage_abi "${arg%%=*}" "${arg#*=}" ;;
    *)   stage_abi "arm64-v8a" "$arg" ;;
  esac
done

# --- 2. java -> dex ----------------------------------------------------------
find "$ROOT/android/java" -name '*.java' > "$OUT/sources.txt"
echo "java sources: $(wc -l < "$OUT/sources.txt")"
# --release, not -source/-target. Those two set the language level only, so
# javac keeps java.lang from the JDK and compiles JDK-only methods without
# complaint; they then throw NoSuchMethodError on the device. Process.pid() and
# Process.isAlive() both got in that way and killed CoreService on every start.
# --release 8 compiles against the Java 8 API surface, which is what an API 24
# device actually offers, so the next such call fails the build instead.
#
# The exit status is captured rather than piped away. `javac ... | grep ... ||
# true` reports grep's status, so a compile error was silently swallowed and
# the build continued on a partial classes/ directory.
if ! javac --release 8 -nowarn -classpath "$AJAR" \
      -d "$OUT/classes" "@$OUT/sources.txt" > "$OUT/javac.log" 2>&1; then
  grep -v 'bootstrap class path' "$OUT/javac.log" || true
  echo "javac failed; see the errors above" >&2
  exit 1
fi
grep -v 'bootstrap class path' "$OUT/javac.log" || true

find "$OUT/classes" -name '*.class' > "$OUT/classes.txt"
"$BT/d8" --lib "$AJAR" --min-api "$MIN_SDK" --output "$OUT" "@$OUT/classes.txt"
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
  --min-sdk-version "$MIN_SDK" \
  --target-sdk-version 35 \
  --version-code 1 \
  --version-name 0.1.0

# --- 4. fold dex + libs into the apk ----------------------------------------
cp "$OUT/base.apk" "$OUT/unsigned.apk"
cp "$OUT/classes.dex" "$OUT/stage/classes.dex"
(
  cd "$OUT/stage"
  # libs.txt is written by stage_abi and is never empty here: a run with nothing
  # to stage exits before reaching this point. Read into an array rather than
  # $(cat ...) so the intended word splitting is explicit and shellcheck's
  # SC2046 stays quiet.
  mapfile -t LIBS < "$OUT/libs.txt"
  zip -q -X "$OUT/unsigned.apk" classes.dex "${LIBS[@]}"
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
