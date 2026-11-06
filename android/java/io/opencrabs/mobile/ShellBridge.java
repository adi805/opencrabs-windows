package io.opencrabs.mobile;

import android.content.pm.PackageManager;
import android.os.IBinder;
import android.os.ParcelFileDescriptor;
import android.util.Log;

import java.io.ByteArrayOutputStream;
import java.io.FileInputStream;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;

import moe.shizuku.server.IRemoteProcess;
import moe.shizuku.server.IShizukuService;
import rikka.shizuku.Shizuku;
import rikka.shizuku.ShizukuBinderWrapper;

/**
 * Runs shell commands as the {@code shell} user through Shizuku.
 *
 * <p>Why this exists at all: the app cannot see or drive another app's UI from
 * its own UID, and the two APIs that can are accessibility (a declaration Play
 * Protect refuses to install from a chat app) and the shell user. Shizuku hands
 * this process a binder to a service that already runs as {@code shell} (UID
 * 2000) or root, and that service can spawn our commands with its own identity.
 *
 * <p>Deliberately NOT using {@code Shizuku.UserService}: that path needs an AIDL
 * round trip plus a service Shizuku starts in a separate process, and this
 * build has no gradle to run the AIDL compiler. The server binder already
 * exposes {@code newProcess}, which is all we need, and it needs no extra
 * component in the manifest.
 *
 * <p>The permission gate is Shizuku's own: {@link #hasPermission()} must be true
 * before any command runs. Shizuku shows that grant dialog itself, once per
 * install, and the user can revoke it in the Shizuku app at any time.
 */
public final class ShellBridge {

    private static final String TAG = "OpenCrabsShell";

    /** Request code for Shizuku's permission dialog. Any stable int works. */
    public static final int PERMISSION_REQUEST = 4242;

    private static volatile boolean sListenerAdded;

    private ShellBridge() {
    }

    /**
     * Subscribes to Shizuku's binder. Must run before {@link #available()}
     * can ever be true: Shizuku delivers the binder asynchronously, so the
     * first call after process start usually reports false even when Shizuku
     * is running. The sticky variant replays an already-received binder, which
     * is why this is safe to call on every activity start.
     */
    public static synchronized void init() {
        if (sListenerAdded) {
            return;
        }
        try {
            Shizuku.addBinderReceivedListenerSticky(new Shizuku.OnBinderReceivedListener() {
                @Override
                public void onBinderReceived() {
                    Log.i(TAG, "shizuku binder received");
                }
            });
            Shizuku.addBinderDeadListener(new Shizuku.OnBinderDeadListener() {
                @Override
                public void onBinderDead() {
                    Log.w(TAG, "shizuku binder died; shell commands are unavailable");
                }
            });
            sListenerAdded = true;
        } catch (Throwable t) {
            // A missing Shizuku install is the common case, not an error. Log
            // once at info level and stay quiet: the automation surface reports
            // the exact reason through /status.
            Log.i(TAG, "shizuku not available: " + t.getClass().getSimpleName());
        }
    }

    /** True when Shizuku is installed AND its server is currently running. */
    public static boolean available() {
        try {
            return Shizuku.pingBinder();
        } catch (Throwable t) {
            return false;
        }
    }

    /** True when this app has been granted Shizuku's shell permission. */
    public static boolean hasPermission() {
        try {
            return Shizuku.checkSelfPermission() == PackageManager.PERMISSION_GRANTED;
        } catch (Throwable t) {
            return false;
        }
    }

    /** Shell/root uid behind the server: 2000 for adb-started, 0 for root. */
    public static int serverUid() {
        try {
            return Shizuku.getUid();
        } catch (Throwable t) {
            return -1;
        }
    }

    /** True when a command can actually be executed right now. */
    public static boolean ready() {
        return available() && hasPermission();
    }

    /**
     * One-line reason the backend cannot run, or null when it can.
     *
     * <p>Load-bearing for honesty: without this the surface answers every
     * request with "ok":false and no explanation, and the agent has to guess
     * whether Shizuku is missing, stopped after a reboot, or merely ungranted.
     */
    public static String explain() {
        if (!available()) {
            return "shizuku is not running: install Shizuku, start it (wireless "
                    + "debugging), then reopen OpenCrabs. Shizuku must be started "
                    + "again after every reboot.";
        }
        if (!hasPermission()) {
            return "shizuku is running but OpenCrabs has no permission: open the "
                    + "Shizuku app and grant it, or tap Allow on the dialog.";
        }
        return null;
    }

    /** Asks Shizuku to show its permission dialog. Safe to call repeatedly. */
    public static void requestPermission() {
        try {
            if (available() && !hasPermission()) {
                Shizuku.requestPermission(PERMISSION_REQUEST);
            }
        } catch (Throwable t) {
            Log.w(TAG, "permission request failed", t);
        }
    }

    // --- exec ---------------------------------------------------------------

    /** Exit code, stdout and stderr of one command. */
    public static final class Result {
        public final int code;
        public final String out;
        public final String err;

        Result(int code, String out, String err) {
            this.code = code;
            this.out = out;
            this.err = err;
        }

        public boolean ok() {
            return code == 0;
        }

        /** stdout when the command succeeded, stderr otherwise. Never null. */
        public String text() {
            if (code == 0) {
                return out;
            }
            return err.isEmpty() ? out : err;
        }
    }

    private static IShizukuService service() throws Exception {
        IBinder binder = Shizuku.getBinder();
        if (binder == null) {
            throw new IllegalStateException("shizuku binder not received");
        }
        // The wrapper is required, not cosmetic: it routes the transaction
        // through Shizuku's own service so the server can attribute the call to
        // this package. A raw transact on the binder is rejected because the
        // server never saw this app attach.
        return IShizukuService.Stub.asInterface(new ShizukuBinderWrapper(binder));
    }

    /**
     * Runs {@code cmd} under {@code sh -c} as the shell user.
     *
     * <p>Drains stdout and stderr on separate threads: a command that writes
     * more than a pipe buffer to stderr would otherwise deadlock while we sit
     * reading stdout, which is the classic ProcessBuilder trap.
     */
    public static Result run(String cmd, long timeoutMs) {
        if (!ready()) {
            return new Result(127, "", explain() == null ? "shizuku not ready" : explain());
        }
        IRemoteProcess proc = null;
        try {
            proc = service().newProcess(new String[] {"sh", "-c", cmd}, null, null);
            final ParcelFileDescriptor outFd = proc.getInputStream();
            final ParcelFileDescriptor errFd = proc.getErrorStream();
            final ByteArrayOutputStream outBuf = new ByteArrayOutputStream();
            final ByteArrayOutputStream errBuf = new ByteArrayOutputStream();

            Thread outThread = drain("crabs-sh-out", outFd, outBuf);
            Thread errThread = drain("crabs-sh-err", errFd, errBuf);

            boolean finished = proc.waitForTimeout(timeoutMs, "MILLISECONDS");
            int code;
            if (finished) {
                code = proc.exitValue();
            } else {
                // Reporting 124 mirrors coreutils timeout: the caller can tell
                // "command failed" from "command never came back".
                proc.destroy();
                code = 124;
            }
            outThread.join(2000);
            errThread.join(2000);
            closeQuietly(outFd);
            closeQuietly(errFd);
            return new Result(code, outBuf.toString("UTF-8"), errBuf.toString("UTF-8"));
        } catch (Throwable t) {
            Log.w(TAG, "command failed: " + cmd, t);
            return new Result(125, "", String.valueOf(t.getMessage()));
        } finally {
            if (proc != null) {
                try {
                    proc.destroy();
                } catch (Throwable ignored) {
                    // already exited, or the binder is gone; either way done
                }
            }
        }
    }

    private static Thread drain(final String name, final ParcelFileDescriptor fd,
                                final ByteArrayOutputStream sink) {
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                InputStream in = null;
                try {
                    in = new FileInputStream(fd.getFileDescriptor());
                    byte[] buf = new byte[8192];
                    int n;
                    while ((n = in.read(buf)) > 0) {
                        sink.write(buf, 0, n);
                    }
                } catch (Throwable ignored) {
                    // stream closed under us when the process died; the partial
                    // output already written is the best answer available
                } finally {
                    if (in != null) {
                        try {
                            in.close();
                        } catch (Throwable ignored) {
                            // nothing useful to do
                        }
                    }
                }
            }
        }, name);
        t.setDaemon(true);
        t.start();
        return t;
    }

    private static void closeQuietly(ParcelFileDescriptor fd) {
        if (fd != null) {
            try {
                fd.close();
            } catch (Throwable ignored) {
                // already closed by the drain thread
            }
        }
    }

    /** Single-quotes an argument for {@code sh -c}. */
    public static String quote(String value) {
        if (value == null) {
            return "''";
        }
        return "'" + value.replace("'", "'\\''") + "'";
    }
}