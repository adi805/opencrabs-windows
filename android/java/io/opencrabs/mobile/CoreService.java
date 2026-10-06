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

import java.io.File;
import java.io.InputStream;
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
        if (core != null && core.isAlive()) {
            Log.i(TAG, "core already running, pid=" + core.pid());
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
            Log.i(TAG, "core started, pid=" + core.pid());
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
}
