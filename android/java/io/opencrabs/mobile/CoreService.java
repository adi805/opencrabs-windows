package io.opencrabs.mobile;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.ApplicationInfo;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;

import java.io.Closeable;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.security.SecureRandom;
import java.util.ArrayList;
import java.util.List;

/**
 * Owns the lifetime of the OpenCrabs core process.
 *
 * <p>The core is a normal Linux executable, not a JNI library: the Rust crate
 * already ships a daemon mode, and running it as a child keeps the UI process
 * free of any blocking work (an ANR here would be a main-thread stall, which is
 * exactly what the PRD forbids).
 *
 * <p>The binary is packaged as {@code lib/arm64-v8a/libopencrabs.so} rather
 * than under a plain name. That is a platform requirement, not a style choice:
 * since Android 10 (API 29) W^X is enforced on the app data directory, so a
 * file written to {@code filesDir} cannot be executed. The app's native
 * library directory is the one place the loader will execute from, and the
 * package manager only extracts entries whose names match {@code lib*.so}.
 */
public class CoreService extends Service {

    private static final String TAG = "OpenCrabsCore";
    private static final String CHANNEL_ID = "opencrabs-core";
    private static final int NOTIFICATION_ID = 1;
    private static final String BINARY_NAME = "libopencrabs.so";

    /**
     * Intent extra: the activity owns the core (it runs the TUI under a pty), so this
     * service must hold the foreground notification without spawning a second core.
     * Two cores on one profile fight over the channel listeners and the database.
     */
    public static final String EXTRA_TUI_MODE = "io.opencrabs.mobile.TUI_MODE";
    /** Where the start mode is kept, so a null-intent restart does not become a daemon. */
    private static final String PREFS_NAME = "opencrabs-service";
    private static final String PREF_TUI_MODE = "tui_mode";

    /**
     * Whether the core should come back on its own after a reboot.
     *
     * Defaults to false on purpose: a process that resurrects itself after every
     * reboot without the operator asking is indistinguishable from malware, and
     * on a phone it also competes for battery with whatever the user actually
     * opened. The Settings screen flips this on.
     */
    static final String PREF_START_ON_BOOT = "start_on_boot";

    static boolean startOnBoot(Context context) {
        return context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
                .getBoolean(PREF_START_ON_BOOT, false);
    }

    static void setStartOnBoot(Context context, boolean enabled) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
                .edit().putBoolean(PREF_START_ON_BOOT, enabled).apply();
    }


    /** The core's config table for the loopback HTTP/JSON surface (PRD FR-006). */
    private static final String SURFACE_TABLE = "[session_surface]";
    /** App-private file holding the generated bearer token. */
    private static final String SURFACE_TOKEN_FILE = "surface_token";

    /** How many consecutive core deaths are restarted before giving up. */
    private static final int MAX_RESTARTS = 5;
    /** First restart delay; doubled per consecutive failure. */
    private static final long RESTART_BACKOFF_MS = 1000L;
    /** Backoff ceiling, so a crash loop that outlives a long run still retries. */
    private static final long MAX_BACKOFF_MS = 60_000L;
    /**
     * Uptime that counts as a working spawn. Past it the restart counter resets,
     * so a core that runs for hours and then dies once does not inherit an old
     * failure count and give up on the first crash.
     */
    private static final long HEALTHY_UPTIME_MS = 60_000L;
    /** How long the exit paths wait for the child to be reaped. */
    private static final long REAP_WAIT_MS = 5_000L;

    private Process core;
    /** Set by onDestroy: a stop the operator asked for must not be restarted. */
    private volatile boolean stopping;
    /** Consecutive restarts since the last spawn that outlived HEALTHY_UPTIME_MS. */
    private int restarts;
    /**
     * Wall clock of the current spawn, for the HEALTHY_UPTIME_MS test. Written
     * by the spawn thread and read by the pump thread, so it is volatile: a
     * stale read here only costs an extra backoff step, but a torn long is not
     * something to leave to the memory model.
     */
    private volatile long spawnedAt;

    /** The directory the package manager extracted our native libs into. */
    public static String nativeDir(Context ctx) {
        ApplicationInfo info = ctx.getApplicationInfo();
        return info.nativeLibraryDir;
    }

    /** Absolute path of the packaged core executable. */
    public static File coreBinary(Context ctx) {
        return new File(nativeDir(ctx), BINARY_NAME);
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    /** Remembers the start mode across sticky restarts, which carry no intent. */
    private SharedPreferences prefs() {
        return getSharedPreferences(PREFS_NAME, MODE_PRIVATE);
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        // A fresh start clears a stop left by a previous instance, otherwise a
        // sticky restart comes up believing it is shutting down and never spawns.
        stopping = false;
        // A START_STICKY restart arrives with a null intent, which used to mean
        // "daemon": the platform would relaunch this service after the app's TUI was
        // already running and spawn a second core on the same profile, colliding on the
        // channel listeners and the sqlite database. The chosen mode is remembered so a
        // restart resumes the mode the operator actually picked.
        boolean tuiMode;
        if (intent != null && intent.hasExtra(EXTRA_TUI_MODE)) {
            tuiMode = intent.getBooleanExtra(EXTRA_TUI_MODE, false);
            prefs().edit().putBoolean(PREF_TUI_MODE, tuiMode).apply();
        } else {
            tuiMode = prefs().getBoolean(PREF_TUI_MODE, false);
            Log.i(TAG, "start without mode extra, resuming tuiMode=" + tuiMode);
        }
        startForeground(NOTIFICATION_ID, buildNotification(tuiMode ? "terminal session" : "starting"));
        // Before the spawn, so the config the core reads already carries the
        // token. The surface starts listening during daemon boot.
        ensureSurfaceToken(this);
        if (tuiMode) {
            // The activity runs the TUI under a pty and owns the core process.
            // Spawning `daemon` here as well would put two cores on one profile:
            // they collide on the channel listeners and on the sqlite database.
            // This service then only holds the foreground notification so the
            // process survives being backgrounded.
            Log.i(TAG, "TUI mode: not spawning a daemon, the terminal owns the core");
            setState("terminal session");
            return START_STICKY;
        }
        spawnCore();
        // START_STICKY: if the platform reclaims us under memory pressure it
        // restarts the service, which is the behaviour the operator expects
        // from something they launched on purpose.
        return START_STICKY;
    }

    private Notification buildNotification(String state) {
        NotificationManager manager = getSystemService(NotificationManager.class);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && manager != null) {
            NotificationChannel channel =
                    new NotificationChannel(CHANNEL_ID, "OpenCrabs core",
                            NotificationManager.IMPORTANCE_LOW);
            manager.createNotificationChannel(channel);
        }
        Notification.Builder builder = (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O)
                ? new Notification.Builder(this, CHANNEL_ID)
                : new Notification.Builder(this);
        return builder
                .setContentTitle("OpenCrabs core")
                .setContentText(state)
                .setSmallIcon(android.R.drawable.stat_notify_sync)
                .setOngoing(true)
                .build();
    }

    /**
     * Republish the ongoing notification so it names the core's actual state.
     *
     * <p>The notification used to be built once with the literal text
     * "running" and never touched again, so a core that had exited was still
     * advertised as running: an operator had no way to tell a live core from a
     * corpse without reading logcat.
     */
    private void setState(String state) {
        NotificationManager manager = getSystemService(NotificationManager.class);
        if (manager == null) {
            Log.w(TAG, "no notification manager, state not published: " + state);
            return;
        }
        manager.notify(NOTIFICATION_ID, buildNotification(state));
    }

    private synchronized void spawnCore() {
        Log.i(TAG, "runtime sdk=" + Build.VERSION.SDK_INT
                + " processPidMethod=" + hasProcessPid());
        // A second start (activity onCreate, then the POST_NOTIFICATIONS callback)
        // resets the label to "starting" a few lines above. So an already-live core
        // has to republish its real state here, or the notification stays stuck on
        // "starting" and reads exactly like a hang.
        if (isAlive(core)) {
            Log.i(TAG, "core already running, pid=" + pidText(core));
            setState("running, pid " + pidText(core));
            return;
        }
        File exe = coreBinary(this);
        if (!exe.exists()) {
            Log.e(TAG, "core binary missing at " + exe.getAbsolutePath());
            setState("core binary missing");
            return;
        }

        List<String> argv = new ArrayList<>();
        argv.add(exe.getAbsolutePath());
        argv.add("daemon");

        // argv is the app's own binary (nativeLibraryDir, set by the package
        // manager) plus a literal subcommand, and ProcessBuilder(List) spawns no
        // shell, so no caller-controlled data reaches a command line. Semgrep's
        // command-injection-process-builder rule flags any non-literal first
        // argument, so the finding is a false positive here.
        // nosemgrep: java.lang.security.audit.command-injection-process-builder.command-injection-process-builder
        ProcessBuilder builder = new ProcessBuilder(argv);
        builder.directory(getFilesDir());
        builder.redirectErrorStream(true);

        // The binary is dynamically linked and needs libc++_shared.so, which
        // sits next to it. A spawned process does not inherit the app's native
        // library search path, so the directory has to be handed over
        // explicitly or the loader fails before main() runs.
        builder.environment().put("LD_LIBRARY_PATH", nativeDir(this));
        // The core resolves its home through dirs::home_dir(), which reads
        // $HOME. It does not read OPENCRABS_HOME: that name appears only in
        // the docs (GETTING_STARTED.md), never in the Rust source, so the
        // config seeded at <filesDir>/home/.opencrabs was never picked up and
        // the session surface stayed disabled. $HOME is what actually points
        // the core at the seeded directory. OPENCRABS_HOME is kept as well so
        // this keeps working if the core ever starts honouring it.
        File coreHome = new File(getFilesDir(), "home");
        coreHome.mkdirs();
        builder.environment().put("HOME", coreHome.getAbsolutePath());
        builder.environment().put("OPENCRABS_HOME", coreHome.getAbsolutePath());

        try {
            core = builder.start();
            spawnedAt = System.currentTimeMillis();
            final Process spawned = core;
            final InputStream out = core.getInputStream();
            Thread pump = new Thread(() -> drain(spawned, out), "core-output");
            pump.setDaemon(true);
            pump.start();
            Log.i(TAG, "core started, pid=" + pidText(core));
            setState("running, pid " + pidText(core));
        } catch (Exception e) {
            Log.e(TAG, "spawn failed", e);
            setState("spawn failed: " + e.getClass().getSimpleName());
            scheduleRestart();
        }
    }

    /**
     * Drain the child's stdout until EOF, then treat that EOF as the core's
     * exit signal.
     *
     * <p>EOF is the only exit notification this process gets for free: a read
     * returning -1 means every writer end of the pipe is gone, so the child is
     * dead. The previous version logged that and returned, which left a dead
     * core behind a notification still reading "running" and nothing left to
     * bring it back.
     */
    private void drain(Process spawned, InputStream out) {
        byte[] buffer = new byte[4096];
        try {
            int read;
            while ((read = out.read(buffer)) > 0) {
                Log.i(TAG, new String(buffer, 0, read));
            }
        } catch (Exception e) {
            Log.w(TAG, "output pump ended", e);
        }
        if (stopping || spawned != core) {
            // An intentional stop, or a newer core already took over: this exit
            // is expected and must not be answered with a restart.
            Log.i(TAG, "output pump ended after stop, pid=" + pidText(spawned));
            return;
        }
        if (System.currentTimeMillis() - spawnedAt >= HEALTHY_UPTIME_MS) {
            // It ran long enough to count as a working spawn, so the next crash
            // starts the backoff over instead of inheriting an old failure count.
            restarts = 0;
        }
        int code = reapExitCode(spawned);
        Log.w(TAG, "core exited, code=" + code);
        scheduleRestart();
    }

    /**
     * Restart the core after a bounded backoff, unless it is stopping or has
     * already spent its attempts.
     */
    private void scheduleRestart() {
        if (stopping) {
            return;
        }
        if (restarts >= MAX_RESTARTS) {
            Log.e(TAG, "core died " + restarts + " times, giving up");
            setState("stopped after " + restarts + " restarts");
            return;
        }
        long delay = backoffMs(restarts);
        restarts++;
        Log.w(TAG, "restarting core in " + delay + "ms, attempt "
                + restarts + "/" + MAX_RESTARTS);
        setState("restarting in " + (delay / 1000L) + "s, attempt "
                + restarts + "/" + MAX_RESTARTS);
        Thread waiter = new Thread(() -> {
            try {
                Thread.sleep(delay);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                return;
            }
            if (!stopping) {
                spawnCore();
            }
        }, "core-restart");
        waiter.setDaemon(true);
        waiter.start();
    }

    /**
     * Exponential backoff, capped at {@link #MAX_BACKOFF_MS}. Attempt 0 waits
     * {@link #RESTART_BACKOFF_MS}.
     *
     * <p>Doubling rather than a fixed delay because the two failure shapes want
     * opposite things: a core that died on something transient (a port still in
     * TIME_WAIT from the previous run) wants the retry soon, while a core that
     * dies instantly every time is a config error that should not spin.
     */
    static long backoffMs(int attempt) {
        long delay = RESTART_BACKOFF_MS;
        for (int i = 0; i < attempt; i++) {
            delay = delay * 2L;
            if (delay >= MAX_BACKOFF_MS) {
                return MAX_BACKOFF_MS;
            }
        }
        return delay;
    }

    /**
     * Wait briefly for the child to be reaped, then report its exit code.
     *
     * <p>The pipe closes before the process table entry is necessarily reaped,
     * so exitValue() right after EOF can still throw IllegalThreadStateException.
     * This polls rather than calling the waitFor(timeout) overload: that one is
     * API 26, and this file has already been bitten once by a java.lang method
     * that javac resolved from the JDK and the device did not have.
     */
    private static int reapExitCode(Process p) {
        long deadline = System.currentTimeMillis() + REAP_WAIT_MS;
        while (System.currentTimeMillis() < deadline) {
            int code = exitCode(p);
            if (code >= 0) {
                return code;
            }
            try {
                Thread.sleep(50L);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                break;
            }
        }
        return exitCode(p);
    }

    /** The child's exit status, or -1 while it is still running. */
    private static int exitCode(Process p) {
        try {
            return p.exitValue();
        } catch (IllegalThreadStateException stillRunning) {
            return -1;
        } catch (Throwable t) {
            return -1;
        }
    }

    @Override
    public void onDestroy() {
        // Before destroy(), because the pump thread reads it: a stop the
        // operator asked for must not come back as a restart.
        stopping = true;
        Process dying = core;
        if (dying != null) {
            dying.destroy();
            // destroy() only asks. Nothing used to confirm the child left, so a
            // core that ignored the signal outlived the service that owned it.
            Log.i(TAG, "core destroyed, exit=" + reapExitCode(dying));
        }
        super.onDestroy();
    }

    // ------------------------------------------------------------- surface --

    /**
     * Generate the loopback surface's bearer token once, and seed it into the
     * core's config.
     *
     * <p>Why the app owns this instead of leaving {@code api_key} unset: the
     * Rust gate in {@code src/session/surface.rs} exempts a token-less listener
     * on a loopback bind, on the reasoning that 127.0.0.1 is reachable only by
     * the same user. That holds on a desktop and fails on Android, where the
     * loopback interface is shared by every app on the device. A token-less
     * surface on 127.0.0.1 is therefore reachable by any other installed app,
     * and that surface can run the agent with tools. So the token is mandatory
     * here, and the app is the only thing that can supply it: the core has no
     * way to know it is running inside an APK.
     *
     * <p>Seeded only when the config does not already declare the table. A
     * second {@code [session_surface]} header would make the file unparseable
     * and the core would then refuse to start, so an existing table is never
     * rewritten.
     *
     * <p>The token file is the hand-off to the session surface client
     * (milestone 3). {@code filesDir} is app-private, so no other app can read
     * the token back out of it.
     */
    static String ensureSurfaceToken(Context ctx) {
        File tokenFile = new File(ctx.getFilesDir(), SURFACE_TOKEN_FILE);
        String token = readText(tokenFile);
        if (token == null || token.isEmpty()) {
            token = newToken();
            writeText(tokenFile, token);
            Log.i(TAG, "surface token generated, " + token.length() + " chars");
        } else {
            Log.i(TAG, "surface token reused from " + tokenFile.getAbsolutePath());
        }
        seedSurfaceConfig(ctx, token);
        return token;
    }

    /**
     * 32 random bytes, hex encoded.
     *
     * <p>Hex rather than Base64 on purpose. {@code java.util.Base64} is API 26
     * and {@code android.util.Base64} is not on the JDK classpath that javac
     * resolves {@code java.*} against, and this file has already been bitten
     * once by an API-level mismatch that compiled cleanly and threw on device
     * (see the process api section below). Hex needs nothing beyond {@code
     * Character.forDigit}, which exists on every API level.
     */
    private static String newToken() {
        byte[] raw = new byte[32];
        new SecureRandom().nextBytes(raw);
        StringBuilder hex = new StringBuilder(raw.length * 2);
        for (byte b : raw) {
            hex.append(Character.forDigit((b >> 4) & 0xf, 16));
            hex.append(Character.forDigit(b & 0xf, 16));
        }
        return hex.toString();
    }

    /** Write {@code [session_surface]} into the config the core will read. */
    private static void seedSurfaceConfig(Context ctx, String token) {
        File dir = new File(new File(ctx.getFilesDir(), "home"), ".opencrabs");
        if (!dir.isDirectory() && !dir.mkdirs()) {
            Log.e(TAG, "cannot create config dir " + dir.getAbsolutePath());
            return;
        }
        File config = new File(dir, "config.toml");
        String existing = readText(config);
        if (existing != null && existing.contains(SURFACE_TABLE)) {
            Log.i(TAG, "config already declares " + SURFACE_TABLE + ", left as-is");
            return;
        }
        StringBuilder seeded = new StringBuilder();
        if (existing != null) {
            seeded.append(existing);
            if (!existing.endsWith("\n")) {
                seeded.append('\n');
            }
        }
        seeded.append('\n')
                .append(SURFACE_TABLE).append('\n')
                .append("enabled = true\n")
                .append("bind = \"127.0.0.1\"\n")
                .append("api_key = \"").append(token).append("\"\n");
        writeText(config, seeded.toString());
        Log.i(TAG, "seeded " + SURFACE_TABLE + " into " + config.getAbsolutePath());
    }

    /** Whole-file read, or null when the file is absent or unreadable. */
    private static String readText(File file) {
        if (!file.isFile()) {
            return null;
        }
        FileInputStream in = null;
        try {
            in = new FileInputStream(file);
            byte[] buffer = new byte[(int) file.length()];
            int offset = 0;
            while (offset < buffer.length) {
                int read = in.read(buffer, offset, buffer.length - offset);
                if (read < 0) {
                    break;
                }
                offset += read;
            }
            return new String(buffer, 0, offset, StandardCharsets.UTF_8);
        } catch (Exception e) {
            Log.w(TAG, "cannot read " + file.getAbsolutePath(), e);
            return null;
        } finally {
            closeQuietly(in);
        }
    }

    private static void writeText(File file, String text) {
        FileOutputStream out = null;
        try {
            out = new FileOutputStream(file);
            out.write(text.getBytes(StandardCharsets.UTF_8));
            out.flush();
        } catch (Exception e) {
            Log.e(TAG, "cannot write " + file.getAbsolutePath(), e);
        } finally {
            closeQuietly(out);
        }
    }

    private static void closeQuietly(Closeable stream) {
        if (stream == null) {
            return;
        }
        try {
            stream.close();
        } catch (Exception ignored) {
            // Nothing left to do: the read or write already succeeded or failed.
        }
    }

    // ---------------------------------------------------------- process api --
    // Process.pid() and Process.isAlive() are NOT part of Android's
    // java.lang.Process. They exist in the JDK, and because assemble-apk.sh runs
    // javac with android.jar on -classpath rather than -bootclasspath, javac
    // resolves java.lang.* from the JDK: the calls compile cleanly and then
    // throw NoSuchMethodError on device. That is exactly what happened on the
    // API 34 emulator (compileSdk 34, so android.jar was correct): the throw
    // escaped onStartCommand, killed the service, and left the already-spawned
    // core orphaned with nobody draining its stdout pipe. Reflect, and fall
    // back to exitValue(), which exists on every API level.

    private static boolean hasProcessPid() {
        try {
            Process.class.getMethod("pid");
            return true;
        } catch (Throwable t) {
            return false;
        }
    }

    private static long pidOf(Process p) {
        if (p == null) {
            return -1L;
        }
        try {
            Object pid = Process.class.getMethod("pid").invoke(p);
            if (pid instanceof Number) {
                return ((Number) pid).longValue();
            }
        } catch (Throwable t) {
            // NoSuchMethodException/NoSuchMethodError on Android: fall through.
        }
        return -1L;
    }

    private static String pidText(Process p) {
        long pid = pidOf(p);
        return pid > 0 ? Long.toString(pid) : "unknown";
    }

    private static boolean isAlive(Process p) {
        if (p == null) {
            return false;
        }
        try {
            Object alive = Process.class.getMethod("isAlive").invoke(p);
            if (alive instanceof Boolean) {
                return (Boolean) alive;
            }
        } catch (Throwable t) {
            // Not available at this API level; use the exitValue() probe below.
        }
        try {
            p.exitValue();
            return false;
        } catch (IllegalThreadStateException stillRunning) {
            return true;
        } catch (Throwable t) {
            return false;
        }
    }
}
