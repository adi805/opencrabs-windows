package io.opencrabs.mobile;

import android.content.Context;
import android.util.Log;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.PrintWriter;
import java.io.StringWriter;

/**
 * Persist the last uncaught exception so the next launch can show it.
 *
 * <p>The app runs on a phone the build box cannot reach with adb, so "it force closed" was
 * undiagnosable: the system dialog names the app and says nothing else, and logcat is not
 * reachable from here. This handler writes the stack trace to internal storage, and the
 * next start renders it on screen, which turns a screenshot into a stack trace.
 *
 * <p>Deliberately self-contained: no androidx, no new dependency, and every failure inside
 * the handler is swallowed because the process is already dying when it runs.
 */
final class CrashLog {

    private static final String TAG = "OpenCrabsCrash";
    private static final String FILE = "last-crash.txt";

    private CrashLog() {}

    /** Install once, as early as possible: the handler covers everything after this call. */
    static void install(final Context context) {
        final Context app = context.getApplicationContext();
        final Thread.UncaughtExceptionHandler previous =
            Thread.getDefaultUncaughtExceptionHandler();
        Thread.setDefaultUncaughtExceptionHandler((thread, error) -> {
            write(app, thread, error);
            if (previous != null) {
                previous.uncaughtException(thread, error);
            }
        });
    }

    private static void write(Context context, Thread thread, Throwable error) {
        try {
            StringWriter buffer = new StringWriter();
            PrintWriter out = new PrintWriter(buffer);
            out.println("thread: " + thread.getName());
            out.println("device: android " + android.os.Build.VERSION.RELEASE
                + " (sdk " + android.os.Build.VERSION.SDK_INT + ") "
                + android.os.Build.MANUFACTURER + " " + android.os.Build.MODEL);
            error.printStackTrace(out);
            out.flush();
            String text = buffer.toString();
            try (FileOutputStream stream =
                     context.openFileOutput(FILE, Context.MODE_PRIVATE)) {
                stream.write(text.getBytes("UTF-8"));
            }
            Log.e(TAG, text);
        } catch (IOException ignored) {
            // Nothing useful to do: the process is already dying.
        }
    }

    /** Last crash text, or null when the previous run was clean. */
    static String last(Context context) {
        File file = new File(context.getFilesDir(), FILE);
        if (!file.isFile()) return null;
        try (FileInputStream stream = new FileInputStream(file)) {
            ByteArrayOutputStream buffer = new ByteArrayOutputStream();
            byte[] chunk = new byte[4096];
            int read;
            while ((read = stream.read(chunk)) > 0) {
                buffer.write(chunk, 0, read);
            }
            String text = buffer.toString("UTF-8");
            return text.isEmpty() ? null : text;
        } catch (IOException error) {
            return "crash log unreadable: " + error;
        }
    }

    static void clear(Context context) {
        new File(context.getFilesDir(), FILE).delete();
    }
}
