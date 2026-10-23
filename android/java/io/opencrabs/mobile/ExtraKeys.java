package io.opencrabs.mobile;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.graphics.Typeface;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
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

    private ExtraKeys() {}

    static View create(Context context, TerminalPane pane) {
        LinearLayout row = new LinearLayout(context);
        row.setOrientation(LinearLayout.HORIZONTAL);
        row.setBackgroundColor(0xff141414);

        addKey(context, row, "ESC", v -> pane.sendBytes(new byte[]{0x1b}));
        addKey(context, row, "TAB", v -> pane.sendBytes(new byte[]{0x09}));
        addKey(context, row, "CTRL", v -> {
            pane.toggleCtrl();
            v.setSelected(pane.isCtrlArmed());
        });
        addKey(context, row, "ALT", v -> {
            pane.toggleAlt();
            v.setSelected(pane.isAltArmed());
        });
        addKey(context, row, "\u2190", v -> pane.sendText("\u001b[D"));
        addKey(context, row, "\u2191", v -> pane.sendText("\u001b[A"));
        addKey(context, row, "\u2193", v -> pane.sendText("\u001b[B"));
        addKey(context, row, "\u2192", v -> pane.sendText("\u001b[C"));
        addKey(context, row, "ENTER", v -> pane.sendBytes(new byte[]{0x0d}));
        addKey(context, row, "PASTE", v -> pane.sendText(readClipboard(context)));
        addKey(context, row, "HIDE", v -> Keyboard.hide(context, pane));

        HorizontalScrollView scroll = new HorizontalScrollView(context);
        scroll.setHorizontalScrollBarEnabled(false);
        scroll.addView(row, new ViewGroup.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        scroll.setBackgroundColor(0xff141414);
        return scroll;
    }

    private interface KeyAction {
        void run(TextView key);
    }

    private static void addKey(Context context, LinearLayout row, String label, KeyAction action) {
        TextView key = new TextView(context);
        key.setText(label);
        key.setTypeface(Typeface.MONOSPACE);
        key.setTextSize(TypedValue.COMPLEX_UNIT_SP, 14);
        key.setTextColor(0xffe6e6e6);
        key.setGravity(Gravity.CENTER);
        int padH = (int) (10 * context.getResources().getDisplayMetrics().density);
        int padV = (int) (6 * context.getResources().getDisplayMetrics().density);
        key.setPadding(padH, padV, padH, padV);
        key.setBackgroundColor(0xff2a2a2a);
        key.setClickable(true);
        key.setFocusable(false);
        key.setOnClickListener(v -> {
            action.run(key);
            key.setTextColor(key.isSelected() ? 0xff7fd1ff : 0xffe6e6e6);
        });
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        int gap = (int) (2 * context.getResources().getDisplayMetrics().density);
        params.setMargins(gap, gap, gap, gap);
        row.addView(key, params);
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
