package io.opencrabs.mobile;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.SharedPreferences;
import android.graphics.Typeface;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.FrameLayout;
import android.widget.HorizontalScrollView;
import android.widget.LinearLayout;
import android.widget.TextView;

/**
 * On-screen key row for {@link TerminalPane}: ESC, TAB, CTRL, ALT, arrows, PASTE, ENTER.
 *
 * <p>A TUI needs keys a phone keyboard does not have, and hardware-key mapping alone leaves
 * the user stranded (no way to send ESC, no arrows, no way to paste a command). Each button
 * writes the exact byte sequence the emulator would have produced for that key.
 */
final class ExtraKeys {

    /** Collapsed/expanded state of the whole key row, kept across launches. */
    private static final String PREFS = "opencrabs_ui";
    private static final String PREF_KEYS_EXPANDED = "extra_keys_expanded";

    private ExtraKeys() {}

    static View create(Context context, TerminalPane pane) {
        LinearLayout row = new LinearLayout(context);
        row.setOrientation(LinearLayout.HORIZONTAL);
        row.setBackgroundColor(0xff141414);

        addKey(context, row, "ESC", v -> pane.sendBytes(new byte[]{0x1b}));
        addKey(context, row, "TAB", v -> pane.sendBytes(new byte[]{0x09}));
        TextView ctrlKey = addKey(context, row, "CTRL", v -> pane.toggleCtrl());
        TextView altKey = addKey(context, row, "ALT", v -> pane.toggleAlt());
        // A modifier is consumed by the keystroke it applies to, so the highlight has to
        // follow the pane rather than only the button's own click.
        Runnable refresh = () -> {
            ctrlKey.setSelected(pane.isCtrlArmed());
            altKey.setSelected(pane.isAltArmed());
            ctrlKey.setTextColor(pane.isCtrlArmed() ? 0xff7fd1ff : 0xffe6e6e6);
            altKey.setTextColor(pane.isAltArmed() ? 0xff7fd1ff : 0xffe6e6e6);
        };
        pane.setModifierListener(refresh);
        refresh.run();
        addKey(context, row, "\u2190", v -> pane.sendText("\u001b[D"));
        addKey(context, row, "\u2191", v -> pane.sendText("\u001b[A"));
        addKey(context, row, "\u2193", v -> pane.sendText("\u001b[B"));
        addKey(context, row, "\u2192", v -> pane.sendText("\u001b[C"));
        addKey(context, row, "ENTER", v -> pane.sendBytes(new byte[]{0x0d}));
        // Through the emulator, so bracketed paste is honoured: writing the text as
        // keystrokes made a multi-line prompt submit at its first newline.
        addKey(context, row, "PASTE", v -> pane.pasteText(readClipboard(context)));
        // Font size is the biggest lever on a phone: a smaller cell fits more columns AND
        // more rows, so this row is the one that decides how much of the TUI is visible.
        addKey(context, row, "A-", v -> pane.stepTextSize(-1));
        addKey(context, row, "A+", v -> pane.stepTextSize(1));
        // Built before the keys so the collapse key at the end of the row can wire
        // itself to them: the row is a child of the scroller, and the strip replaces
        // the scroller in the same slot.
        HorizontalScrollView scroll = new HorizontalScrollView(context);
        scroll.setHorizontalScrollBarEnabled(false);
        scroll.addView(row, new ViewGroup.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        scroll.setBackgroundColor(0xff141414);

        // Collapsed handle. A bare GONE row would leave no way back while the title bar
        // is hidden for typing, so the row collapses to a strip instead of vanishing.
        TextView handle = new TextView(context);
        handle.setText("\u2303 keys");
        handle.setTextSize(TypedValue.COMPLEX_UNIT_SP, 10);
        handle.setTextColor(0xff9a9a9a);
        handle.setGravity(Gravity.CENTER);
        handle.setBackgroundColor(0xff141414);
        handle.setClickable(true);
        handle.setFocusable(false);
        handle.setPadding(0, dp(context, 2), 0, dp(context, 2));

        FrameLayout holder = new FrameLayout(context);
        holder.addView(scroll, new FrameLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        holder.addView(handle, new FrameLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));

        final SharedPreferences prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        addKey(context, row, "\u25BE", v -> {
            prefs.edit().putBoolean(PREF_KEYS_EXPANDED, false).apply();
            applyExpanded(scroll, handle, false);
        });
        addKey(context, row, "HIDE", v -> Keyboard.hide(context, pane));
        handle.setOnClickListener(v -> {
            prefs.edit().putBoolean(PREF_KEYS_EXPANDED, true).apply();
            applyExpanded(scroll, handle, true);
        });

        applyExpanded(scroll, handle, prefs.getBoolean(PREF_KEYS_EXPANDED, true));
        return holder;
    }

    private static void applyExpanded(View scroll, View handle, boolean expanded) {
        scroll.setVisibility(expanded ? View.VISIBLE : View.GONE);
        handle.setVisibility(expanded ? View.GONE : View.VISIBLE);
    }

    private static int dp(Context context, int value) {
        return (int) (value * context.getResources().getDisplayMetrics().density);
    }

    private interface KeyAction {
        void run(TextView key);
    }

    private static TextView addKey(Context context, LinearLayout row, String label, KeyAction action) {
        TextView key = new TextView(context);
        key.setText(label);
        key.setTypeface(Typeface.MONOSPACE);
        key.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        key.setTextColor(0xffe6e6e6);
        key.setGravity(Gravity.CENTER);
        // Padding stays small (7/3) so the row costs little vertical space, but the
        // touch target is enforced separately: 12sp text on 3dp padding draws about
        // 20dp tall, well under the 48dp Material minimum and under the 32dp that
        // RustDesk's mobile terminal keyboard uses. A key you miss costs more taps
        // than a few dp of chrome, so minWidth/minHeight carry the target.
        int padH = (int) (7 * context.getResources().getDisplayMetrics().density);
        int padV = (int) (3 * context.getResources().getDisplayMetrics().density);
        key.setPadding(padH, padV, padH, padV);
        key.setMinWidth(dp(context, 40));
        key.setMinHeight(dp(context, 32));
        key.setBackgroundColor(0xff2a2a2a);
        key.setClickable(true);
        key.setFocusable(false);
        key.setOnClickListener(v -> {
            action.run(key);
            key.setTextColor(key.isSelected() ? 0xff7fd1ff : 0xffe6e6e6);
        });
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        int gap = (int) (1 * context.getResources().getDisplayMetrics().density);
        params.setMargins(gap, gap, gap, gap);
        row.addView(key, params);
        return key;
    }

    private static String readClipboard(Context context) {
        ClipboardManager manager = (ClipboardManager) context.getSystemService(Context.CLIPBOARD_SERVICE);
        if (manager == null || !manager.hasPrimaryClip()) return "";
        ClipData clip = manager.getPrimaryClip();
        if (clip == null || clip.getItemCount() == 0) return "";
        CharSequence text = clip.getItemAt(0).coerceToText(context);
        return text == null ? "" : text.toString();
    }
}
