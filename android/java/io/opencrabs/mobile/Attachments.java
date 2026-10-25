package io.opencrabs.mobile;

import android.content.ContentResolver;
import android.content.Context;
import android.database.Cursor;
import android.net.Uri;
import android.provider.OpenableColumns;
import android.util.Log;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;

/**
 * Copies a file the user picked into the core's own home, so the core can read it.
 *
 * <p>A {@code content://} uri from the system picker is granted to this app for the
 * lifetime of the activity at most, and the core is a separate process that resolves paths
 * through its own {@code $HOME}. Handing it a uri would either fail or stop working later,
 * so the bytes are copied once into {@code files/home/uploads} and the core is given a
 * plain absolute path. The core's home is inside this app's private storage, so nothing
 * outside the app can read the copy back.
 *
 * <p>This is the same marker contract the chat channels use: {@code <<IMG:/path>>} and
 * {@code <<VID:/path>>} tell the agent which tool to call, and any other file is handed
 * over as a bare path so it can be parsed as a document.
 */
final class Attachments {

    private static final String TAG = "OpenCrabsAttach";

    /** Where picked files land, relative to the core's $HOME. */
    private static final String UPLOAD_DIR = "uploads";

    private Attachments() {}

    /**
     * Copy {@code uri} into the core's home and return the marker to type into the prompt.
     *
     * @return the text to insert, or null when the copy failed
     */
    static String importToHome(Context context, Uri uri) {
        String name = displayName(context, uri);
        File dir = new File(new File(context.getFilesDir(), "home"), UPLOAD_DIR);
        if (!dir.isDirectory() && !dir.mkdirs()) {
            Log.e(TAG, "cannot create " + dir.getAbsolutePath());
            return null;
        }
        File target = uniqueTarget(dir, name);
        if (!copy(context, uri, target)) {
            return null;
        }
        String path = target.getAbsolutePath();
        Log.i(TAG, "imported " + path + " (" + target.length() + " bytes)");
        return marker(path);
    }

    /** Media gets the marker the agent recognises; everything else is a bare path. */
    static String marker(String path) {
        String lower = path.toLowerCase(java.util.Locale.ROOT);
        if (lower.endsWith(".jpg") || lower.endsWith(".jpeg") || lower.endsWith(".png")
            || lower.endsWith(".webp") || lower.endsWith(".gif")
            || lower.endsWith(".bmp") || lower.endsWith(".heic")) {
            return "<<IMG:" + path + ">>";
        }
        if (lower.endsWith(".mp4") || lower.endsWith(".mov") || lower.endsWith(".webm")
            || lower.endsWith(".mkv") || lower.endsWith(".3gp")
            || lower.endsWith(".avi") || lower.endsWith(".flv")) {
            return "<<VID:" + path + ">>";
        }
        return path;
    }

    /**
     * The provider's display name, sanitised.
     *
     * <p>The name comes from another app, so it is untrusted: a slash would move the copy
     * into a directory of the provider's choosing, and ".." would climb out of the upload
     * directory. Everything outside a conservative set is replaced rather than escaped,
     * because the core only needs a readable name, not the original one.
     */
    private static String displayName(Context context, Uri uri) {
        String name = null;
        Cursor cursor = null;
        try {
            cursor = context.getContentResolver().query(uri, null, null, null, null);
            if (cursor != null && cursor.moveToFirst()) {
                int column = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                if (column >= 0) {
                    name = cursor.getString(column);
                }
            }
        } catch (Exception e) {
            Log.w(TAG, "cannot read the display name", e);
        } finally {
            if (cursor != null) {
                cursor.close();
            }
        }
        if (name == null || name.trim().isEmpty()) {
            name = "attachment";
        }
        StringBuilder safe = new StringBuilder(name.length());
        for (int i = 0; i < name.length(); i++) {
            char c = name.charAt(i);
            boolean ok = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
                || (c >= '0' && c <= '9') || c == '.' || c == '-' || c == '_';
            safe.append(ok ? c : '_');
        }
        String cleaned = safe.toString();
        // A name that is only dots would still resolve to a directory traversal.
        if (cleaned.replace(".", "").isEmpty()) {
            return "attachment";
        }
        return cleaned;
    }

    /** Never overwrite an earlier attachment: the agent may still be reading it. */
    private static File uniqueTarget(File dir, String name) {
        File candidate = new File(dir, name);
        if (!candidate.exists()) {
            return candidate;
        }
        int dot = name.lastIndexOf('.');
        String stem = dot > 0 ? name.substring(0, dot) : name;
        String extension = dot > 0 ? name.substring(dot) : "";
        for (int i = 2; i < 1000; i++) {
            candidate = new File(dir, stem + "-" + i + extension);
            if (!candidate.exists()) {
                return candidate;
            }
        }
        return new File(dir, System.currentTimeMillis() + extension);
    }

    private static boolean copy(Context context, Uri uri, File target) {
        InputStream in = null;
        FileOutputStream out = null;
        try {
            ContentResolver resolver = context.getContentResolver();
            in = resolver.openInputStream(uri);
            if (in == null) {
                Log.e(TAG, "provider returned no stream for " + uri);
                return false;
            }
            out = new FileOutputStream(target);
            byte[] buffer = new byte[64 * 1024];
            int read;
            while ((read = in.read(buffer)) > 0) {
                out.write(buffer, 0, read);
            }
            out.flush();
            return true;
        } catch (Exception e) {
            Log.e(TAG, "copy failed for " + uri, e);
            //noinspection ResultOfMethodCallIgnored
            target.delete();
            return false;
        } finally {
            if (in != null) {
                try {
                    in.close();
                } catch (Exception ignored) {
                    // The copy result is already decided.
                }
            }
            if (out != null) {
                try {
                    out.close();
                } catch (Exception ignored) {
                    // Same as above.
                }
            }
        }
    }
}
