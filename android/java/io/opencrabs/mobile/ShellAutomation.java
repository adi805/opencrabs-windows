package io.opencrabs.mobile;

import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.util.Log;
import android.util.Xml;

import org.json.JSONArray;
import org.json.JSONObject;
import org.xmlpull.v1.XmlPullParser;

import java.io.StringReader;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;

/**
 * Drives other apps' UI over the shell user, via {@link ShellBridge}.
 *
 * <p>This replaces the accessibility service that used to live here. Same
 * primitives, same JSON shape, different engine:
 *
 * <ul>
 *   <li>{@code /dump} runs {@code uiautomator dump} and parses the XML it
 *       writes. Accessibility gave the live node tree; this gives the snapshot
 *       uiautomator serialises, which is a subset (no action list, no live
 *       updates) but carries text, ids, bounds and the clickable/scrollable
 *       flags, which is what the agent actually reads.</li>
 *   <li>Tap, swipe, type and the global actions are the platform's own
 *       {@code input} tool, which only works for a process holding
 *       INJECT_EVENTS. That is exactly what the shell UID gives us.</li>
 *   <li>Launch uses {@code monkey} rather than startActivity on purpose: a
 *       background app may not start an activity (silently blocked since
 *       Android 10), while a shell-issued monkey is not subject to that.</li>
 * </ul>
 *
 * <p>Cost of the swap, stated plainly: each action spawns a process, so this is
 * slower than an in-process gesture dispatch, and Shizuku does not survive a
 * reboot without being restarted by hand. Those are the trade for an APK that
 * installs from a chat app without Play Protect refusing it.
 */
public final class ShellAutomation {

    private static final String TAG = "OpenCrabsShellA11y";

    /** Depth and node caps so a pathological tree cannot blow up the caller. */
    private static final int MAX_NODES = 3000;

    /** uiautomator needs a writable path; /data/local/tmp is shell-writable. */
    private static final String DUMP_PATH = "/data/local/tmp/crabs-window.xml";

    private static final long DUMP_TIMEOUT_MS = 20000;
    private static final long ACTION_TIMEOUT_MS = 8000;

    private final Context mContext;

    public ShellAutomation(Context context) {
        mContext = context.getApplicationContext();
    }

    /** True when a command can run; the caller reports {@link #explain()} when not. */
    public static boolean ready() {
        return ShellBridge.ready();
    }

    public static String explain() {
        return ShellBridge.explain();
    }

    // --- read ---------------------------------------------------------------

    /**
     * Flattens the current window into the same array shape the accessibility
     * implementation produced, so callers and prompts do not change.
     */
    public JSONArray dumpScreen() throws Exception {
        String cmd = "uiautomator dump " + DUMP_PATH + " >/dev/null 2>&1; cat " + DUMP_PATH;
        ShellBridge.Result r = ShellBridge.run(cmd, DUMP_TIMEOUT_MS);
        if (!r.ok()) {
            throw new IllegalStateException("uiautomator dump failed: " + r.text().trim());
        }
        String xml = r.out.trim();
        int start = xml.indexOf("<?xml");
        if (start < 0) {
            start = xml.indexOf("<hierarchy");
        }
        if (start < 0) {
            throw new IllegalStateException("uiautomator produced no XML: "
                    + (xml.isEmpty() ? "empty output" : xml.substring(0, Math.min(120, xml.length()))));
        }
        return parse(xml.substring(start));
    }

    private JSONArray parse(String xml) throws Exception {
        JSONArray out = new JSONArray();
        XmlPullParser p = Xml.newPullParser();
        p.setFeature(XmlPullParser.FEATURE_PROCESS_NAMESPACES, false);
        p.setInput(new StringReader(xml));

        int nodes = 0;
        int event = p.getEventType();
        while (event != XmlPullParser.END_DOCUMENT) {
            if (event == XmlPullParser.START_TAG && "node".equals(p.getName())) {
                if (++nodes > MAX_NODES) {
                    break;
                }
                JSONObject row = toRow(p, nodes);
                if (row != null) {
                    out.put(row);
                }
            }
            event = p.next();
        }
        return out;
    }

    private static JSONObject toRow(XmlPullParser p, int index) throws Exception {
        String text = attr(p, "text");
        String desc = attr(p, "content-desc");
        String cls = attr(p, "class");
        String pkg = attr(p, "package");

        boolean clickable = "true".equals(attr(p, "clickable"));
        boolean scrollable = "true".equals(attr(p, "scrollable"));
        boolean checkable = "true".equals(attr(p, "checkable"));
        boolean checked = "true".equals(attr(p, "checked"));
        boolean enabled = !"false".equals(attr(p, "enabled"));
        boolean editable = isEditable(cls);

        String label = firstNonEmpty(text, desc);
        boolean actionable = clickable || scrollable || checkable || editable;
        // Same filter as before: an unlabelled layout container is noise that
        // costs the caller context and buys nothing.
        if (label == null && !actionable) {
            return null;
        }

        int[] bounds = parseBounds(attr(p, "bounds"));
        JSONObject row = new JSONObject();
        row.put("i", index);
        if (label != null) {
            row.put("text", label);
        }
        row.put("cls", shortClass(cls));
        row.put("pkg", pkg == null ? "" : pkg);
        row.put("b", new JSONArray(bounds));
        if (clickable) row.put("click", true);
        if (editable) row.put("edit", true);
        if (scrollable) row.put("scroll", true);
        if (checkable) row.put("checkable", true);
        if (checked) row.put("checked", true);
        if (!enabled) row.put("disabled", true);
        return row;
    }

    private static String attr(XmlPullParser p, String name) {
        return p.getAttributeValue(null, name);
    }

    /** uiautomator writes bounds as "[left,top][right,bottom]". */
    private static int[] parseBounds(String raw) {
        int[] zero = new int[] {0, 0, 0, 0};
        if (raw == null) {
            return zero;
        }
        StringBuilder digits = new StringBuilder();
        int[] values = new int[4];
        int k = 0;
        for (int i = 0; i < raw.length() && k < 4; i++) {
            char c = raw.charAt(i);
            if (c >= '0' && c <= '9') {
                digits.append(c);
            } else if (digits.length() > 0) {
                values[k++] = Integer.parseInt(digits.toString());
                digits.setLength(0);
            }
        }
        if (k == 4) {
            return values;
        }
        if (k == 3 && digits.length() > 0) {
            values[3] = Integer.parseInt(digits.toString());
            return values;
        }
        return zero;
    }

    private static boolean isEditable(String cls) {
        if (cls == null) {
            return false;
        }
        return cls.contains("EditText") || cls.contains("AutoCompleteTextView");
    }

    private static String firstNonEmpty(String a, String b) {
        if (a != null) {
            String s = a.trim();
            if (!s.isEmpty()) {
                return s;
            }
        }
        if (b != null) {
            String s = b.trim();
            if (!s.isEmpty()) {
                return s;
            }
        }
        return null;
    }

    private static String shortClass(String cls) {
        if (cls == null) {
            return "";
        }
        int dot = cls.lastIndexOf('.');
        return dot >= 0 ? cls.substring(dot + 1) : cls;
    }

    // --- act ----------------------------------------------------------------

    public boolean tap(float x, float y) {
        return runOk("input tap " + Math.round(x) + " " + Math.round(y));
    }

    public boolean swipe(float x1, float y1, float x2, float y2, long durationMs) {
        long ms = durationMs <= 0 ? 300 : durationMs;
        return runOk("input swipe " + Math.round(x1) + " " + Math.round(y1) + " "
                + Math.round(x2) + " " + Math.round(y2) + " " + ms);
    }

    /**
     * Types into whatever has focus.
     *
     * <p>{@code input text} feeds characters through the key map, so a space
     * must be sent as {@code %s} and characters the input method cannot
     * represent are dropped by the platform. It also appends rather than
     * replaces. Both are limits of the shell route, reported here rather than
     * papered over.
     */
    public boolean setText(String text) {
        if (text == null) {
            return false;
        }
        String encoded = text.replace(" ", "%s");
        return runOk("input text " + ShellBridge.quote(encoded));
    }

    public boolean global(String action) {
        if (action == null) {
            return false;
        }
        String a = action.trim().toLowerCase();
        if ("back".equals(a)) {
            return keyevent(4);
        }
        if ("home".equals(a)) {
            return keyevent(3);
        }
        if ("recents".equals(a)) {
            return keyevent(187);
        }
        if ("notifications".equals(a)) {
            return runOk("cmd statusbar expand-notifications");
        }
        return false;
    }

    private boolean keyevent(int code) {
        return runOk("input keyevent " + code);
    }

    /** Finds the first labelled row containing {@code needle} and taps its centre. */
    public boolean clickText(String needle) throws Exception {
        if (needle == null || needle.trim().isEmpty()) {
            return false;
        }
        String want = needle.trim().toLowerCase();
        JSONArray nodes = dumpScreen();
        for (int i = 0; i < nodes.length(); i++) {
            JSONObject row = nodes.getJSONObject(i);
            String label = row.optString("text", "");
            if (label.isEmpty() || !label.toLowerCase().contains(want)) {
                continue;
            }
            JSONArray b = row.optJSONArray("b");
            if (b == null || b.length() < 4) {
                continue;
            }
            float cx = (b.getInt(0) + b.getInt(2)) / 2f;
            float cy = (b.getInt(1) + b.getInt(3)) / 2f;
            return tap(cx, cy);
        }
        return false;
    }

    /**
     * Opens an installed app.
     *
     * <p>monkey instead of startActivity: a background app cannot start an
     * activity, and the block is silent, so startActivity would answer "ok"
     * having done nothing. The shell user is not subject to it.
     */
    public boolean launch(String pkg) {
        if (pkg == null || pkg.trim().isEmpty()) {
            return false;
        }
        String target = pkg.trim();
        ShellBridge.Result r = ShellBridge.run(
                "monkey -p " + ShellBridge.quote(target)
                        + " -c android.intent.category.LAUNCHER 1",
                ACTION_TIMEOUT_MS);
        if (!r.ok()) {
            return false;
        }
        String all = r.out + " " + r.err;
        return !all.contains("No activities found") && !all.contains("monkey aborted");
    }

    /** Launchable packages, resolved from this app's own PackageManager. */
    public List<String> launchablePackages() {
        Set<String> found = new LinkedHashSet<String>();
        try {
            Intent main = new Intent(Intent.ACTION_MAIN);
            main.addCategory(Intent.CATEGORY_LAUNCHER);
            List<ResolveInfo> resolved = mContext.getPackageManager()
                    .queryIntentActivities(main, 0);
            for (ResolveInfo info : resolved) {
                if (info.activityInfo != null && info.activityInfo.packageName != null) {
                    found.add(info.activityInfo.packageName);
                }
            }
        } catch (Throwable t) {
            Log.w(TAG, "package query failed", t);
        }
        List<String> out = new ArrayList<String>(found);
        Collections.sort(out);
        return out;
    }

    private static boolean runOk(String cmd) {
        ShellBridge.Result r = ShellBridge.run(cmd, ACTION_TIMEOUT_MS);
        if (!r.ok()) {
            Log.w(TAG, "command failed (" + r.code + "): " + cmd + " -> " + r.text().trim());
        }
        return r.ok();
    }

    /** Whether the automation surface may answer any endpoint right now. */
    public static boolean permissionGranted() {
        return ShellBridge.hasPermission();
    }

    /** UID behind the voice: 2000 (adb) or 0 (root), for /status. */
    public static int serverUid() {
        return ShellBridge.serverUid();
    }

    /** True when Shizuku itself is running, independent of permission. */
    public static boolean shizukuRunning() {
        return ShellBridge.available();
    }

}