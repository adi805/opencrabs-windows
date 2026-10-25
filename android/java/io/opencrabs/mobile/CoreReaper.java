package io.opencrabs.mobile;

import android.util.Log;

import java.io.File;
import java.io.FileInputStream;

/**
 * Kills core processes that outlived the app that spawned them.
 *
 * <p>The TUI runs the core under a pty owned by the activity, so the core is a child of
 * the app process. When Android kills the app (low memory, "close all", a crash), the
 * child is reparented to init and keeps running: it still holds the profile's instance
 * lock and its channels. The next launch then starts a second core, the new one tries to
 * preempt the old one, the old one does not stop within the two seconds the preempt path
 * allows, and the session dies with {@code exit 75} ("already running for profile
 * 'default' ... and did not stop"). The app looks permanently broken until the user kills
 * the process by hand.
 *
 * <p>Why SIGKILL instead of the core's own shutdown: the orphan's stdout is a pty whose
 * master side died with the app, so a graceful shutdown that tries to write its last frame
 * blocks on a full pipe and never reaches exit. Nothing is lost by killing it, because it
 * cannot paint anywhere the user can see.
 *
 * <p>Only orphans are touched. A process whose parent is still alive belongs to a running
 * app instance (the foreground service spawns its own core in non-TUI mode), so it is left
 * alone: the ppid is the discriminator, not the binary name.
 */
final class CoreReaper {

    private static final String TAG = "OpenCrabsReaper";

    /** The core binary is staged as a native library, so its path carries this. */
    private static final String BINARY_MARKER = "libopencrabs.so";

    private CoreReaper() {}

    /**
     * Kill every orphaned core and report how many were killed.
     *
     * @return the number of processes killed, for logging and for tests
     */
    static int reapOrphans() {
        File proc = new File("/proc");
        String[] entries = proc.list();
        if (entries == null) {
            return 0;
        }
        int killed = 0;
        for (String entry : entries) {
            int pid = parsePid(entry);
            if (pid <= 0 || pid == android.os.Process.myPid()) {
                continue;
            }
            File dir = new File(proc, entry);
            String cmdline = readNulSeparated(new File(dir, "cmdline"));
            if (cmdline == null || !cmdline.contains(BINARY_MARKER)) {
                continue;
            }
            // ppid 1 is init: nobody is waiting for this process any more.
            if (parentPid(new File(dir, "stat")) != 1) {
                continue;
            }
            Log.i(TAG, "reaping orphaned core pid=" + pid + " cmd=" + cmdline.trim());
            try {
                android.os.Process.sendSignal(pid, 9);
                killed++;
            } catch (Exception e) {
                // A pid that vanished between the scan and the signal is the normal
                // case for a race with init reaping it, not a failure worth surfacing.
                Log.w(TAG, "cannot signal pid " + pid, e);
            }
        }
        return killed;
    }

    /** Numeric /proc entry names are pids; anything else is a kernel pseudo-file. */
    private static int parsePid(String name) {
        if (name.isEmpty()) {
            return -1;
        }
        for (int i = 0; i < name.length(); i++) {
            char c = name.charAt(i);
            if (c < '0' || c > '9') {
                return -1;
            }
        }
        try {
            return Integer.parseInt(name);
        } catch (NumberFormatException e) {
            return -1;
        }
    }

    /** /proc/&lt;pid&gt;/cmdline separates argv entries with NUL; keep it readable. */
    private static String readNulSeparated(File file) {
        byte[] raw = readAll(file, 512);
        if (raw == null) {
            return null;
        }
        StringBuilder text = new StringBuilder(raw.length);
        for (byte b : raw) {
            text.append(b == 0 ? ' ' : (char) (b & 0xff));
        }
        return text.toString();
    }

    /**
     * The ppid out of /proc/&lt;pid&gt;/stat, or -1 when unreadable.
     *
     * <p>Parsed from the last ')' because the second field is the executable name in
     * parentheses and it may itself contain spaces or parentheses.
     */
    private static int parentPid(File stat) {
        byte[] raw = readAll(stat, 512);
        if (raw == null) {
            return -1;
        }
        String text = new String(raw);
        int close = text.lastIndexOf(')');
        if (close < 0 || close + 2 >= text.length()) {
            return -1;
        }
        // After ") " the fields are: state ppid pgrp ...
        String[] rest = text.substring(close + 2).trim().split("\\s+");
        if (rest.length < 2) {
            return -1;
        }
        try {
            return Integer.parseInt(rest[1]);
        } catch (NumberFormatException e) {
            return -1;
        }
    }

    private static byte[] readAll(File file, int cap) {
        FileInputStream in = null;
        try {
            in = new FileInputStream(file);
            byte[] buffer = new byte[cap];
            int read = in.read(buffer);
            if (read <= 0) {
                return null;
            }
            byte[] exact = new byte[read];
            System.arraycopy(buffer, 0, exact, 0, read);
            return exact;
        } catch (Exception e) {
            // /proc entries for other uids, or a pid that exited mid-scan.
            return null;
        } finally {
            if (in != null) {
                try {
                    in.close();
                } catch (Exception ignored) {
                    // Read already decided the result.
                }
            }
        }
    }
}
