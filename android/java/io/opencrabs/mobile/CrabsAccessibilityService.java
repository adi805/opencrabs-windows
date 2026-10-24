package io.opencrabs.mobile;

import android.accessibilityservice.AccessibilityService;
import android.accessibilityservice.GestureDescription;
import android.graphics.Path;
import android.graphics.Rect;
import android.os.Build;
import android.util.Log;
import android.view.accessibility.AccessibilityEvent;
import android.view.accessibility.AccessibilityNodeInfo;

import org.json.JSONArray;
import org.json.JSONObject;

import java.util.ArrayList;
import java.util.List;

/**
 * Reads and drives OTHER apps' UI trees.
 *
 * <p>This is the only Android API that can see another app's view hierarchy:
 * an app cannot inspect or click outside its own process without it. That power
 * is exactly why the service is opt-in, disabled by default, and cannot be
 * enabled from code. The user must turn it on in Settings > Accessibility, and
 * the platform keeps a visible switch there that revokes it at any time.
 *
 * <p>Scope discipline, deliberately narrow: this class exposes primitives
 * (read the tree, tap a point, tap a label, type, swipe, press back/home) and
 * nothing else. It contains no per-app recipes, no scheduling, and no loop that
 * keeps hammering a screen. Bulk actions against a third-party service
 * (auto-follow, mass DM, engagement farming) violate most platforms' terms and
 * get the user's account banned, so those are a decision for the caller, not
 * something this service bakes in and hides.
 */
public class CrabsAccessibilityService extends AccessibilityService {

    private static final String TAG = "OpenCrabsA11y";

    /** Depth and node caps so a pathological tree cannot hang the main thread. */
    private static final int MAX_DEPTH = 40;
    private static final int MAX_NODES = 3000;

    private static volatile CrabsAccessibilityService sInstance;

    public static CrabsAccessibilityService getInstance() {
        return sInstance;
    }

    public static boolean isConnected() {
        return sInstance != null;
    }

    @Override
    protected void onServiceConnected() {
        super.onServiceConnected();
        sInstance = this;
        Log.i(TAG, "accessibility service connected");
    }

    @Override
    public void onDestroy() {
        sInstance = null;
        Log.i(TAG, "accessibility service destroyed");
        super.onDestroy();
    }

    @Override
    public void onAccessibilityEvent(AccessibilityEvent event) {
        // Intentionally empty. Subscribing to the event stream and reacting to
        // every window change is what makes an accessibility service expensive:
        // it receives events from every app, all the time. This service is
        // pull-based, the caller asks for the tree when it wants it, so it costs
        // nothing while idle.
    }

    @Override
    public void onInterrupt() {
        // No long-running feedback loop to interrupt.
    }

    // --- read ---------------------------------------------------------------

    /** Flattens the active window into a compact array of nodes with bounds. */
    public JSONArray dumpScreen() throws Exception {
        JSONArray out = new JSONArray();
        AccessibilityNodeInfo root = getRootInActiveWindow();
        if (root == null) {
            return out;
        }
        try {
            walk(root, out, 0, new int[] {0});
        } finally {
            // Nodes obtained from a service must be recycled on older APIs or
            // the framework leaks them until GC pressure forces a sweep.
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
                root.recycle();
            }
        }
        return out;
    }

    private void walk(AccessibilityNodeInfo node, JSONArray out, int depth, int[] budget)
            throws Exception {
        if (node == null || depth > MAX_DEPTH || budget[0] >= MAX_NODES) {
            return;
        }
        budget[0]++;

        CharSequence text = node.getText();
        CharSequence desc = node.getContentDescription();
        CharSequence hint = Build.VERSION.SDK_INT >= Build.VERSION_CODES.O
                ? node.getHintText() : null;

        String label = firstNonEmpty(text, desc, hint);
        boolean actionable = node.isClickable() || node.isEditable()
                || node.isScrollable() || node.isCheckable();

        // Only nodes that carry a label or can be acted on are reported. A raw
        // dump of every layout container is thousands of rows of noise and
        // blows up the caller's context for nothing.
        if (label != null || actionable) {
            JSONObject row = new JSONObject();
            Rect r = new Rect();
            node.getBoundsInScreen(r);
            row.put("i", budget[0]);
            if (label != null) {
                row.put("text", label);
            }
            row.put("cls", shortClass(node.getClassName()));
            row.put("pkg", String.valueOf(node.getPackageName()));
            row.put("b", new JSONArray(new int[] {r.left, r.top, r.right, r.bottom}));
            if (node.isClickable()) row.put("click", true);
            if (node.isEditable()) row.put("edit", true);
            if (node.isScrollable()) row.put("scroll", true);
            if (node.isCheckable()) row.put("checkable", true);
            if (node.isChecked()) row.put("checked", true);
            if (!node.isEnabled()) row.put("disabled", true);
            out.put(row);
        }

        int n = node.getChildCount();
        for (int i = 0; i < n; i++) {
            AccessibilityNodeInfo child = null;
            try {
                child = node.getChild(i);
                walk(child, out, depth + 1, budget);
            } catch (Exception e) {
                // A child can be detached between getChild and the read. Skip it
                // rather than aborting the whole dump.
            } finally {
                if (child != null && Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
                    child.recycle();
                }
            }
            if (budget[0] >= MAX_NODES) {
                return;
            }
        }
    }

    private static String firstNonEmpty(CharSequence... values) {
        for (CharSequence v : values) {
            if (v != null) {
                String s = v.toString().trim();
                if (!s.isEmpty()) {
                    return s;
                }
            }
        }
        return null;
    }

    private static String shortClass(CharSequence cls) {
        if (cls == null) {
            return "";
        }
        String s = cls.toString();
        int dot = s.lastIndexOf('.');
        return dot >= 0 ? s.substring(dot + 1) : s;
    }

    // --- act ----------------------------------------------------------------

    /** Finds the first node whose label contains {@code needle}, ignoring case. */
    public AccessibilityNodeInfo findByText(String needle) {
        AccessibilityNodeInfo root = getRootInActiveWindow();
        if (root == null) {
            return null;
        }
        return findByText(root, needle, 0, new int[] {0});
    }

    private AccessibilityNodeInfo findByText(AccessibilityNodeInfo node, String needle,
                                             int depth, int[] budget) {
        if (node == null || depth > MAX_DEPTH || budget[0] >= MAX_NODES) {
            return null;
        }
        budget[0]++;

        String label = firstNonEmpty(node.getText(), node.getContentDescription());
        if (label != null && label.toLowerCase().contains(needle.toLowerCase())) {
            return node;
        }

        int n = node.getChildCount();
        for (int i = 0; i < n; i++) {
            AccessibilityNodeInfo child = null;
            try {
                child = node.getChild(i);
                AccessibilityNodeInfo hit = findByText(child, needle, depth + 1, budget);
                if (hit != null) {
                    return hit;
                }
            } catch (Exception e) {
                // detached child, skip
            }
            if (budget[0] >= MAX_NODES) {
                return null;
            }
        }
        return null;
    }

    /**
     * Taps a point using a synthesized gesture.
     *
     * <p>A gesture rather than node.performAction(ACTION_CLICK) because plenty of
     * real screens (custom canvases, web views, games) expose no clickable node
     * at the point you can see. Coordinates are screen pixels, matching what
     * dumpScreen reports, so a caller can read bounds and tap them directly.
     */
    public boolean tap(float x, float y) {
        Path p = new Path();
        p.moveTo(x, y);
        GestureDescription.StrokeDescription stroke =
                new GestureDescription.StrokeDescription(p, 0, 60);
        GestureDescription.Builder b = new GestureDescription.Builder();
        b.addStroke(stroke);
        return dispatchGesture(b.build(), null, null);
    }

    /** Swipes from one point to another over {@code durationMs}. */
    public boolean swipe(float x1, float y1, float x2, float y2, long durationMs) {
        Path p = new Path();
        p.moveTo(x1, y1);
        p.lineTo(x2, y2);
        GestureDescription.StrokeDescription stroke =
                new GestureDescription.StrokeDescription(p, 0, Math.max(50, durationMs));
        GestureDescription.Builder b = new GestureDescription.Builder();
        b.addStroke(stroke);
        return dispatchGesture(b.build(), null, null);
    }

    /** Types into the focused editable node. */
    public boolean setText(String text) {
        AccessibilityNodeInfo root = getRootInActiveWindow();
        if (root == null) {
            return false;
        }
        AccessibilityNodeInfo focused = root.findFocus(AccessibilityNodeInfo.FOCUS_INPUT);
        if (focused == null) {
            return false;
        }
        android.os.Bundle args = new android.os.Bundle();
        args.putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, text);
        return focused.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args);
    }

    /** Clicks a node by label. Returns false when nothing matches. */
    public boolean clickText(String needle) {
        AccessibilityNodeInfo node = findByText(needle);
        if (node == null) {
            return false;
        }
        // Walk up to the nearest clickable ancestor: the labelled TextView is
        // usually a child of the row that actually handles the tap.
        AccessibilityNodeInfo cur = node;
        for (int i = 0; i < 8 && cur != null; i++) {
            if (cur.isClickable()) {
                return cur.performAction(AccessibilityNodeInfo.ACTION_CLICK);
            }
            cur = cur.getParent();
        }
        Rect r = new Rect();
        node.getBoundsInScreen(r);
        if (r.width() > 0 && r.height() > 0) {
            return tap(r.exactCenterX(), r.exactCenterY());
        }
        return false;
    }

    /** Fires a global action: back, home, recents, notifications. */
    public boolean global(String action) {
        switch (action) {
            case "back":    return performGlobalAction(GLOBAL_ACTION_BACK);
            case "home":    return performGlobalAction(GLOBAL_ACTION_HOME);
            case "recents": return performGlobalAction(GLOBAL_ACTION_RECENTS);
            case "notifications": return performGlobalAction(GLOBAL_ACTION_NOTIFICATIONS);
            default: return false;
        }
    }

    /** Launches an app by package name. */
    public boolean launch(String pkg) {
        android.content.Intent i = getPackageManager().getLaunchIntentForPackage(pkg);
        if (i == null) {
            return false;
        }
        i.addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK);
        try {
            startActivity(i);
            return true;
        } catch (Exception e) {
            Log.w(TAG, "launch failed for " + pkg, e);
            return false;
        }
    }

    /** Lists installed launchable packages, so a caller can resolve a name. */
    public List<String> launchablePackages() {
        List<String> out = new ArrayList<>();
        android.content.Intent probe = new android.content.Intent(android.content.Intent.ACTION_MAIN);
        probe.addCategory(android.content.Intent.CATEGORY_LAUNCHER);
        for (android.content.pm.ResolveInfo ri :
                getPackageManager().queryIntentActivities(probe, 0)) {
            String p = ri.activityInfo.packageName;
            if (!out.contains(p)) {
                out.add(p);
            }
        }
        return out;
    }
}
