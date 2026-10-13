package io.opencrabs.mobile;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
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

    /** The core's config table for the loopback HTTP/JSON surface (PRD FR-006). */
    private static final String SURFACE_TABLE = "[session_surface]";
    /** App-private file holding the generated bearer token. */
    private static final String SURFACE_TOKEN_FILE = "surface_token";

    private Process core;

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

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        startForeground(NOTIFICATION_ID, buildNotification());
        // Before the spawn, so the config the core reads already carries the
        // token. The surface starts listening during daemon boot.
        ensureSurfaceToken(this);
        spawnCore();
        // START_STICKY: if the platform reclaims us under memory pressure it
        // restarts the service, which is the behaviour the operator expects
        // from something they launched on purpose.
        return START_STICKY;
    }

    private Notification buildNotification() {
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
                .setContentText("running")
                .setSmallIcon(android.R.drawable.stat_notify_sync)
                .setOngoing(true)
                .build();
    }

    private void spawnCore() {
        Log.i(TAG, "runtime sdk=" + Build.VERSION.SDK_INT
                + " processPidMethod=" + hasProcessPid());
        if (isAlive(core)) {
            Log.i(TAG, "core already running, pid=" + pidText(core));
            return;
        }
        File exe = coreBinary(this);
        if (!exe.exists()) {
            Log.e(TAG, "core binary missing at " + exe.getAbsolutePath());
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
            final InputStream out = core.getInputStream();
            Thread pump = new Thread(() -> {
                byte[] buffer = new byte[4096];
                try {
                    int read;
                    while ((read = out.read(buffer)) > 0) {
                        Log.i(TAG, new String(buffer, 0, read));
                    }
                } catch (Exception e) {
                    Log.w(TAG, "output pump ended", e);
                }
            }, "core-output");
            pump.setDaemon(true);
            pump.start();
            Log.i(TAG, "core started, pid=" + pidText(core));
        } catch (Exception e) {
            Log.e(TAG, "spawn failed", e);
        }
    }

    @Override
    public void onDestroy() {
        if (core != null) {
            core.destroy();
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
