#!/usr/bin/env bash
#
# On-device smoke test for the Android client, run against a booted emulator.
#
# Why this exists: every Android check in this repo so far is static. The build
# job proves the artifact is an Android ELF (file + readelf), the packaging job
# proves the APK carries the libs. Neither proves that Android will actually
# execute it. This script is the missing half: it installs the APK on a real
# system image, launches it, and then checks the things only a running device
# can answer.
#
# What it answers, in order of importance:
#   1. Does the core ELF execute under Android's loader? (Bionic, not glibc.)
#   2. Does libc++_shared.so resolve at spawn time? It is not a system lib, so
#      a missing copy is a hard dlopen failure.
#   3. Does the daemon survive, and at what RSS? A phone kills processes under
#      memory pressure; this is the number that decides whether that happens.
#   4. Does the process wedge? The #55 stall was a userspace deadlock, so
#      liveness is sampled twice, 60s apart, not assumed from one probe.
#
# Deliberately does NOT use `set -e`: a failure in one check must not hide the
# evidence from the others. Failures are counted and the exit code reflects
# them at the end, after every dump has been written.
#
# Usage: android-emulator-smoke.sh <path-to-apk> [output-dir]
set -uo pipefail

APK="${1:?usage: $0 <path-to-apk> [output-dir]}"
OUT="${2:-/tmp/android-emulator-smoke}"
PKG="io.opencrabs.mobile"
ACTIVITY="$PKG/.MainActivity"
PORT=18792
HOME_DIR="/data/data/$PKG/files/home"

mkdir -p "$OUT"
FAILURES=0

say() { printf '\n=== %s ===\n' "$1"; }
fail() {
  printf 'FAIL: %s\n' "$1"
  FAILURES=$((FAILURES + 1))
}
pass() { printf 'PASS: %s\n' "$1"; }

# ---------------------------------------------------------------- device ----
say "device"
adb wait-for-device
SDK="$(adb shell getprop ro.build.version.sdk | tr -d '\r')"
ABI="$(adb shell getprop ro.product.cpu.abi | tr -d '\r')"
ABILIST="$(adb shell getprop ro.product.cpu.abilist | tr -d '\r')"
echo "sdk      : $SDK"
echo "abi      : $ABI"
echo "abilist  : $ABILIST"
echo "apk      : $APK ($(stat -c%s "$APK" 2>/dev/null || echo '?') bytes)"

# ------------------------------------------------------------------ root ----
# The emulator images used here are google_apis (no Play Store), which are
# rootable. Root is how the config seed below reaches the app's private data
# dir without adding android:debuggable to the shipped manifest.
say "root"
# adb root races device registration. The emulator can report boot complete
# while adbd still refuses commands on this serial, and a single-shot call then
# dies with "device not found". That happened on run 37541308719: root was
# reported unavailable, the seed was skipped, and the two surface checks failed
# for a reason that lived entirely in this harness. So retry until the shell
# answers as uid 0, and decide from `id -u` rather than adb's own wording,
# which differs between platform-tools versions.
ROOTED=0
for _ in $(seq 1 30); do
  adb root > "$OUT/adb-root.txt" 2>&1 || true
  if [ "$(adb shell id -u 2>/dev/null | tr -d '\r')" = "0" ]; then
    ROOTED=1
    adb wait-for-device
    break
  fi
  sleep 2
done
cat "$OUT/adb-root.txt"
if [ "$ROOTED" = "1" ]; then
  pass "adb root (shell uid 0)"
else
  # Counted as a failure, not a warning: without root the seed cannot be
  # written, the surface is never enabled, and the surface checks below would
  # report a failure that belongs to this script rather than to Android.
  fail "adb root unavailable after 60s, the surface config cannot be seeded"
fi

# --------------------------------------------------------------- install ----
say "install"
adb install -r -t "$APK" > "$OUT/install.txt" 2>&1 || true
cat "$OUT/install.txt"
if adb shell pm list packages 2>/dev/null | tr -d '\r' | grep -q "^package:$PKG$"; then
  pass "package installed"
else
  fail "package $PKG is not installed"
fi

# ------------------------------------------------------------------ seed ----
# The session surface is the thing under test; [agent] debug_logs is set only
# so the core writes a log the dump below can read. Every other setting stays at
# the core's own default, so a failure here still points at the surface, not at
# unrelated config.
if [ "$ROOTED" = "1" ]; then
  say "seed config (enable the session surface)"
  adb shell "mkdir -p $HOME_DIR/.opencrabs" > "$OUT/seed.txt" 2>&1 || true
  adb shell "cat > $HOME_DIR/.opencrabs/config.toml" >> "$OUT/seed.txt" 2>&1 <<'CFG'
# Written by .github/scripts/android-emulator-smoke.sh.
[session_surface]
enabled = true
bind = "127.0.0.1"
port = 18792

# The core logs nowhere by default: `logging.file` is None, the file layer is
# gated per-event on `agent.debug_logs`, and the console layer writes to
# io::sink unless `console_output` is set. So the core-log dump below reported
# "the core did not get far enough to write one" on every run - a false
# conclusion, since the core reaches its banner and simply had no sink. Turn
# file logging on so the surface decision is actually recorded.
[agent]
debug_logs = true
CFG
  # The commands above run as root, so everything they create is owned by root.
  # The core runs as the app uid and has to WRITE here (it creates opencrabs.db
  # and logs/), so a root-owned directory stops it starting and the surface then
  # never answers - a failure caused by this harness, not by Android. Hand the
  # tree to the app uid and restore the SELinux label, which root-created files
  # under /data/data also get wrong.
  APP_UID="$(adb shell stat -c %u "/data/data/$PKG" 2>/dev/null | tr -d '\r')"
  if [ -n "$APP_UID" ]; then
    adb shell "chown -R $APP_UID:$APP_UID $HOME_DIR" >> "$OUT/seed.txt" 2>&1 || true
    adb shell "restorecon -R $HOME_DIR" >> "$OUT/seed.txt" 2>&1 || true
    pass "seed handed to the app uid ($APP_UID)"
  else
    fail "could not read the app uid, the seed stays root-owned"
  fi
  adb shell "cat $HOME_DIR/.opencrabs/config.toml" 2>&1 | tee "$OUT/config.toml"
  if grep -q "enabled = true" "$OUT/config.toml"; then
    pass "surface config seeded"
  else
    fail "surface config was not written"
  fi
fi

# ---------------------------------------------------------------- launch ----
say "launch"
adb logcat -c > /dev/null 2>&1 || true
adb shell am start -n "$ACTIVITY" > "$OUT/am-start.txt" 2>&1 || true
cat "$OUT/am-start.txt"
sleep 20

# ------------------------------------------------------------- processes ----
say "processes"
adb shell ps -A > "$OUT/ps.txt" 2>&1 || true
APP_PID="$(adb shell pidof "$PKG" 2>/dev/null | tr -d '\r')"
CORE_PID="$(adb shell pidof libopencrabs.so 2>/dev/null | tr -d '\r')"
if [ -z "$CORE_PID" ]; then
  # pidof matches comm, which the kernel truncates to 15 chars. libopencrabs.so
  # is exactly 15, so it should match, but fall back to scanning the process
  # table rather than reporting "no core" on a naming technicality.
  CORE_PID="$(grep -i 'libopencrabs' "$OUT/ps.txt" | awk '{print $2}' | head -1 | tr -d '\r')"
fi
echo "app  pid : ${APP_PID:-<none>}"
echo "core pid : ${CORE_PID:-<none>}"
grep -i 'opencrabs' "$OUT/ps.txt" || true
if [ -n "$APP_PID" ]; then pass "app process alive"; else fail "app process not running"; fi
if [ -n "$CORE_PID" ]; then pass "core process alive"; else fail "core process not running"; fi

# ---------------------------------------------------------------- logcat ----
say "logcat (OpenCrabsCore)"
adb logcat -d -s OpenCrabsCore:V > "$OUT/logcat-core.txt" 2>&1 || true
head -60 "$OUT/logcat-core.txt"
if grep -q "core started, pid=" "$OUT/logcat-core.txt"; then
  pass "CoreService reported a spawned core"
else
  fail "no 'core started, pid=' line from CoreService"
fi
if grep -qiE 'dlopen failed|cannot execute|spawn failed|Permission denied|not found' "$OUT/logcat-core.txt"; then
  fail "loader or spawn error in the core log"
else
  pass "no loader or spawn error"
fi

# -------------------------------------------------------------- core log ----
# The core's own log is the only place that records WHY the surface is or is
# not up: the config it actually loaded, any bind refusal, and how far startup
# got. Without it a failed run reports "surface did not answer" and nothing
# else, which is how three emulator runs were spent guessing at the cause.
# Dumped here, before the liveness probe, so the reason is on screen above the
# verdict instead of buried in an artifact nobody opens.
say "core home and log"
adb shell "ls -la $HOME_DIR/.opencrabs" > "$OUT/core-home.txt" 2>&1 || true
adb shell "ls -la $HOME_DIR/.opencrabs/logs" >> "$OUT/core-home.txt" 2>&1 || true
cat "$OUT/core-home.txt"
CORE_LOG="$(adb shell "ls -t $HOME_DIR/.opencrabs/logs/opencrabs.* 2>/dev/null | head -1" | tr -d '\r')"
echo "core log: ${CORE_LOG:-<none>}"
if [ -n "$CORE_LOG" ]; then
  adb shell "cat $CORE_LOG" > "$OUT/core-log.txt" 2>&1 || true
  echo "--- core log: config and surface lines ---"
  grep -iE 'unknown keys|session_surface|surface|bind|listen|refus' "$OUT/core-log.txt" | tail -25 || true
  echo "--- core log: last 30 lines ---"
  tail -30 "$OUT/core-log.txt"
else
  echo "NOTE: no core log under $HOME_DIR/.opencrabs/logs. The seed sets [agent] debug_logs = true, so an empty dir means the core never loaded that config (or never reached logging init) - not that the surface was reached and refused."
fi

# ------------------------------------------------- surface liveness probe ----
# Run before and after the RSS window. A single probe proves the socket opened;
# two probes 60s apart are what distinguish "running" from "wedged".
# Does the core hold the port at all? /proc/net/tcp is world-readable, so this
# needs no root, and it separates "the surface never bound" (a config or core
# bug) from "it bound but did not answer" (a request bug). Through `adb forward`
# both arrive as curl error 52, which is exactly why three emulator runs could
# not tell them apart. A table we cannot read is reported as inconclusive rather
# than as a failure, so an unreadable /proc never invents a red result.
say "device port $PORT"
NET_TCP="$(adb shell "cat /proc/net/tcp /proc/net/tcp6 2>/dev/null" 2>/dev/null | tr -d '\r')"
if printf '%s\n' "$NET_TCP" | grep -qiE ":$(printf '%04X' "$PORT")[[:space:]]"; then
  pass "the core holds tcp:$PORT on the device"
elif [ -z "$NET_TCP" ]; then
  echo "NOTE: could not read /proc/net/tcp on the device; the port binding was not checked"
elif [ "$ROOTED" = "1" ]; then
  fail "nothing holds tcp:$PORT on the device (the surface never bound)"
else
  echo "NOTE: nothing holds tcp:$PORT, expected: the config was not seeded (no root)"
fi

say "surface (t=0)"
adb forward "tcp:$PORT" "tcp:$PORT" > /dev/null 2>&1 || true
HEALTH_BEFORE=0
for _ in $(seq 1 15); do
  if curl -sS --max-time 5 "http://127.0.0.1:$PORT/surface/health" \
      -o "$OUT/health-before.json" 2> "$OUT/curl-before.err"; then
    HEALTH_BEFORE=1
    break
  fi
  sleep 4
done
if [ "$HEALTH_BEFORE" = "1" ]; then
  pass "surface answered at t=0"
  cat "$OUT/health-before.json"
else
  fail "surface did not answer at t=0"
  cat "$OUT/curl-before.err" 2>/dev/null || true
fi

# ------------------------------------------------------------------- rss ----
# Sampled, not read once: a growing RSS is the signal that matters for the
# "will this get killed on a phone" question, and a single reading cannot show
# growth. 6 samples x 10s = 60s.
say "rss samples (60s)"
: > "$OUT/rss.txt"
for i in 1 2 3 4 5 6; do
  if [ -n "$CORE_PID" ]; then
    {
      printf 't=%02ds ' "$((i * 10))"
      adb shell "grep -E 'VmRSS|VmSize|Threads' /proc/$CORE_PID/status" 2>/dev/null \
        | tr -d '\r' | tr '\n' ' '
      printf '\n'
    } | tee -a "$OUT/rss.txt"
  fi
  sleep 10
done
cat "$OUT/rss.txt"

say "surface (t=60s)"
HEALTH_AFTER=0
for _ in $(seq 1 10); do
  if curl -sS --max-time 5 "http://127.0.0.1:$PORT/surface/health" \
      -o "$OUT/health-after.json" 2> "$OUT/curl-after.err"; then
    HEALTH_AFTER=1
    break
  fi
  sleep 4
done
CORE_PID_AFTER="$(adb shell pidof libopencrabs.so 2>/dev/null | tr -d '\r')"
echo "core pid after 60s : ${CORE_PID_AFTER:-<none>}"
if [ "$HEALTH_AFTER" = "1" ]; then
  pass "surface still answered at t=60s"
  cat "$OUT/health-after.json"
else
  # Distinguish "never started" from "started then wedged". Both are failures
  # but they are different bugs: a wedge means the surface answered once and
  # then stopped (the #55 class of stall), while never answering at all means
  # the surface was never reached or refused to bind, which the core log dumped
  # above now explains. Calling the second one a "possible wedge" sends the
  # reader hunting a deadlock that was never there.
  if [ "$HEALTH_BEFORE" = "1" ]; then
    fail "surface answered at t=0 then stopped within 60s (possible wedge)"
  else
    fail "surface never answered (see the core log above)"
  fi
  cat "$OUT/curl-after.err" 2>/dev/null || true
fi
if [ -n "$CORE_PID_AFTER" ]; then
  pass "core survived the 60s window"
else
  fail "core process is gone after 60s"
fi

# ------------------------------------------------------------- crash logs ----
say "crash and ANR buffers"
adb logcat -d -b crash > "$OUT/logcat-crash.txt" 2>&1 || true
echo "crash buffer lines: $(wc -l < "$OUT/logcat-crash.txt")"
# Print the buffer whole. The stack trace is the only thing that says WHY the
# app died, and the grep below keeps just the header line, so a run that fails
# here reports "FATAL EXCEPTION" and nothing else. Cost is a few dozen lines.
echo "--- crash buffer (full) ---"
cat "$OUT/logcat-crash.txt"
adb logcat -d > "$OUT/logcat-full.txt" 2>&1 || true
grep -iE 'ANR in|FATAL EXCEPTION|Force finishing' "$OUT/logcat-full.txt" > "$OUT/logcat-anr.txt" || true
cat "$OUT/logcat-anr.txt"
if [ -s "$OUT/logcat-anr.txt" ]; then
  fail "ANR or fatal exception in the full log"
else
  pass "no ANR or fatal exception"
fi

# ---------------------------------------------------------------- summary ----
say "summary"
printf 'sdk=%s abi=%s app_pid=%s core_pid=%s core_pid_after=%s failures=%s\n' \
  "$SDK" "$ABI" "${APP_PID:-none}" "${CORE_PID:-none}" "${CORE_PID_AFTER:-none}" "$FAILURES"
printf 'artifacts in %s\n' "$OUT"
ls -l "$OUT"

if [ "$FAILURES" -gt 0 ]; then
  echo
  echo "RESULT=FAIL ($FAILURES checks failed)"
  exit 1
fi
echo
echo "RESULT=PASS"
