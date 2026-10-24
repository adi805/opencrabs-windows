#!/usr/bin/env bash
#
# Assemble a minimal APK around the Android core binary.
#
# Hand-rolled instead of Gradle on purpose: this needs a manifest, one dex and
# three .so files. Pulling in AGP would add an AGP/JDK/SDK version-compat surface
# for no benefit, and build-tools is already on the runner image.
#
# The three native libs are libopencrabs.so (the core, built by cargo),
# libc++_shared.so (its C++ runtime, taken from the NDK) and libopencrabspty.so
# (the forkpty JNI bridge in android/jni/pty.c, compiled here). The first two are
# per-ABI copies; the third is compiled per ABI because a shared object built for
# aarch64 cannot be loaded by an x86_64 process.
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
#
# Signing: CI signs with the release key from repo secrets
# (ANDROID_KEYSTORE_B64 + _PASS/_ALIAS/_KEY_PASS), because Play Protect
# blocks debug-signed installs on real phones. android/debug.keystore stays
# as the fallback so secretless local builds remain installable and keep one
# stable certificate for `adb install -r` upgrades.
set -euo pipefail

# minSdk 26 is a product contract, not a build detail: the PRD pins it in
# "Layer 2: Machine Spec" and NFR-005, and AC-026 verifies it on a real
# device. It is also the floor the Java shell actually needs (NotificationChannel,
# plus Process.isAlive/destroyForcibly). Declared once so the dex and the
# manifest cannot drift apart, which is how it read 24 in both places before.
MIN_SDK=26

# The Android client has its own version train, independent of the core crate's
# version in Cargo.toml: this artifact is a shell that bundles a core build, and
# the shell changes on its own schedule (0.2.0 is the release that turns it from
# a daemon wrapper into a terminal). Declared once for the same reason MIN_SDK
# is: as two bare literals they can drift, and a versionCode that does not
# increase is not a cosmetic bug, it is an APK Android refuses to install over
# the previous one.
VERSION_CODE=3
VERSION_NAME=0.2.1

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
  # "$@" and not "$1": the caller globs, so the versions arrive as separate
  # arguments. Reading only $1 returned the FIRST match (34.0.0) while the
  # comment claimed "latest", which made sort -V dead code and pinned the
  # build to the oldest build-tools on the image.
  ls -d "$@" 2>/dev/null | sort -V | tail -1
}

pick_build_tools() {
  # Newest build-tools that actually ships every binary this script calls.
  # Newest alone is not enough: a version dir can exist without the tools, and
  # the runner image gains versions over time. Fall back rather than fail.
  for d in $(ls -d "$SDK"/build-tools/*/ 2>/dev/null | sort -Vr); do
    if [ -x "${d}aapt2" ] && [ -x "${d}d8" ] \
      && [ -x "${d}zipalign" ] && [ -x "${d}apksigner" ]; then
      printf '%s\n' "${d%/}"
      return 0
    fi
  done
  return 1
}

BT="$(pick_build_tools)"
if [ -z "$BT" ]; then
  echo "no build-tools under $SDK ships aapt2+d8+zipalign+apksigner" >&2
  exit 2
fi
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

# clang wants the 32-bit ARM triple spelled armv7a-*, while the NDK's prebuilt
# libc++ directory is named arm-linux-androideabi. Two spellings for one target,
# so they get two functions instead of one clever one.
clang_triple() {
  case "$1" in
    arm64-v8a)   echo "aarch64-linux-android" ;;
    armeabi-v7a) echo "armv7a-linux-androideabi" ;;
    x86)         echo "i686-linux-android" ;;
    x86_64)      echo "x86_64-linux-android" ;;
    *)           echo "" ;;
  esac
}

# The NDK's clang, wherever the runner unpacked it. Returns 1 rather than
# printing a bogus path: the pty library is not optional, so the caller has to
# be able to tell "no clang" from "clang failed".
find_clang() {
  if [ -z "$NDK" ] || [ ! -d "$NDK" ]; then
    return 1
  fi
  for c in "$NDK"/toolchains/llvm/prebuilt/*/bin/clang; do
    if [ -x "$c" ]; then
      printf '%s\n' "$c"
      return 0
    fi
  done
  return 1
}

# Resolved once, up front: the pty bridge is required, so a runner without an
# NDK has to fail here with a clear message instead of halfway through staging.
CLANG="$(find_clang || true)"

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

  # The forkpty bridge. Compiled per ABI: a shared object is machine code, and
  # the loader refuses an aarch64 .so inside an x86_64 process. -shared with no
  # -l flags because pty.c only touches libc (no liblog: it does not log).
  ct="$(clang_triple "$abi")"
  if [ -z "$ct" ]; then
    echo "unknown ABI '$abi': no clang target triple, cannot build libopencrabspty.so" >&2
    exit 2
  fi
  if [ -z "$CLANG" ]; then
    echo "no NDK clang found (NDK='${NDK:-<unset>}'): libopencrabspty.so cannot be built" >&2
    echo "set ANDROID_NDK_HOME/ANDROID_NDK_ROOT to an NDK with toolchains/llvm/prebuilt" >&2
    exit 2
  fi
  if [ ! -f "$ROOT/android/jni/pty.c" ]; then
    echo "missing $ROOT/android/jni/pty.c" >&2
    exit 2
  fi
  # --target pins the ABI *and* the API level, so bionic's forkpty declaration
  # is visible (it arrived in API 23; this app's floor is 26).
  "$CLANG" --target="${ct}${MIN_SDK}" -O2 -fPIC -shared \
    -o "$OUT/stage/lib/$abi/libopencrabspty.so" \
    "$ROOT/android/jni/pty.c"
  if [ ! -f "$OUT/stage/lib/$abi/libopencrabspty.so" ]; then
    echo "clang produced no libopencrabspty.so for $abi" >&2
    exit 2
  fi
  chmod 755 "$OUT/stage/lib/$abi/libopencrabspty.so"
  echo "pty bridge  : $abi <- clang ($ct$MIN_SDK, $(stat -c%s "$OUT/stage/lib/$abi/libopencrabspty.so") bytes)"

  printf '%s\n' "lib/$abi/libopencrabs.so" "lib/$abi/libc++_shared.so" \
    "lib/$abi/libopencrabspty.so" >> "$OUT/libs.txt"
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
# android/res holds the launcher icons. aapt2 has no "point me at a resource
# directory" flag on `link`: -R means "overlay resources from this APK", so
# passing the directory made aapt2 try to read it as a zip and die with
# "failed to read magic from input: Is a directory". Resources are compiled to
# .flat first and handed to link as positional inputs, which is the only shape
# link accepts.
if [ ! -d "$ROOT/android/res" ]; then
  echo "android/res is missing but the manifest references @mipmap/ic_launcher" >&2
  exit 1
fi
rm -rf "$OUT/res"
mkdir -p "$OUT/res"
"$BT/aapt2" compile --dir "$ROOT/android/res" -o "$OUT/res"
mapfile -t FLATS < <(find "$OUT/res" -type f -name '*.flat' | sort)
if [ "${#FLATS[@]}" -eq 0 ]; then
  echo "aapt2 compile produced no .flat files from android/res" >&2
  exit 1
fi
echo "resources   : ${#FLATS[@]} flat files"

"$BT/aapt2" link \
  -o "$OUT/base.apk" \
  --manifest "$ROOT/android/AndroidManifest.xml" \
  -I "$AJAR" \
  --min-sdk-version "$MIN_SDK" \
  --target-sdk-version 35 \
  --version-code "$VERSION_CODE" \
  --version-name "$VERSION_NAME" \
  "${FLATS[@]}"

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

# The signing key must be STABLE across builds. Generating a fresh keypair per
# build gives every APK a different signature, so `adb install -r` fails with
# INSTALL_FAILED_UPDATE_INCOMPATIBLE and the user has to uninstall first, which
# discards the app's config and database. Two sources, in order:
#
#   1. ANDROID_KEYSTORE_B64 (+ _PASS / _ALIAS / _KEY_PASS): a base64 keystore
#      from CI secrets. Use this for anything handed to a user.
#   2. android/debug.keystore: committed on purpose. It is the standard Android
#      debug key - password "android", publicly known, zero secret value - and
#      it is here so CI builds are reproducible instead of random.
#
# A debug-signed build cannot be upgraded in place by a release-signed one:
# Android treats a different certificate as a different app, so that switch
# needs one uninstall. Keeping the debug key stable at least means our own
# successive builds upgrade in place.
#
# The release path is not cosmetic. Google Play Protect scans every APK the
# installer feeds it before the user can tap through, and the debug
# certificate is a publicly known constant: a debug-signed app that binds
# an accessibility service and asks to be exempted from battery
# optimization matched "Aplikasi diblokir untuk melindungi perangkat Anda"
# on a real phone, with only an "Oke" button and no install-anyway path
# (v0.2.1, 2026-10-24). A private release certificate is no guarantee of a
# clean scan, but it removes the strongest signal, which is that every
# copy of this APK presents the same public key as repackaged malware.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
if [ -n "${ANDROID_KEYSTORE_B64:-}" ]; then
  KS="$OUT/signing.keystore"
  printf '%s' "$ANDROID_KEYSTORE_B64" | base64 -d > "$KS"
  : "${ANDROID_KEYSTORE_PASS:?ANDROID_KEYSTORE_B64 is set but ANDROID_KEYSTORE_PASS is not}"
  : "${ANDROID_KEY_ALIAS:?ANDROID_KEYSTORE_B64 is set but ANDROID_KEY_ALIAS is not}"
  KS_PASS="$ANDROID_KEYSTORE_PASS"
  KS_ALIAS="$ANDROID_KEY_ALIAS"
  KEY_PASS="${ANDROID_KEY_PASS:-$ANDROID_KEYSTORE_PASS}"
else
  KS="$SCRIPT_DIR/debug.keystore"
  if [ ! -f "$KS" ]; then
    echo "no signing key: set ANDROID_KEYSTORE_B64, or restore android/debug.keystore" >&2
    exit 1
  fi
  KS_PASS="android"
  KS_ALIAS="androiddebugkey"
  KEY_PASS="android"
fi

"$BT/apksigner" sign \
  --ks "$KS" \
  --ks-key-alias "$KS_ALIAS" \
  --ks-pass "pass:$KS_PASS" \
  --key-pass "pass:$KEY_PASS" \
  --out "$OUT/opencrabs-release.apk" \
  "$OUT/aligned.apk"

"$BT/apksigner" verify --print-certs "$OUT/opencrabs-release.apk" | head -3

# --- 6. verify what we actually built ---------------------------------------
echo
echo "=== badging ==="
"$BT/aapt2" dump badging "$OUT/opencrabs-release.apk" 2>/dev/null \
  | grep -E "^(package|sdkVersion|targetSdkVersion|application-label|launchable-activity|native-code)" || true
echo
echo "=== zip contents (libs must be present, named lib*.so) ==="
unzip -l "$OUT/opencrabs-release.apk" | grep -E "classes.dex|lib/" || true

# A staged lib that never reached the APK is the failure this asserts against:
# everything above would still print success, and the crash would only appear
# on a device as UnsatisfiedLinkError inside System.loadLibrary.
for required in libopencrabs.so libc++_shared.so libopencrabspty.so; do
  if ! unzip -l "$OUT/opencrabs-release.apk" | grep -q "lib/.*/$required\$"; then
    echo "APK is missing $required" >&2
    exit 1
  fi
done
echo "native libs : all three present in the APK"
echo
echo "apk: $OUT/opencrabs-release.apk ($(stat -c%s "$OUT/opencrabs-release.apk") bytes)"
