package io.opencrabs.mobile;

import android.content.Context;
import android.view.View;
import android.view.inputmethod.InputMethodManager;

/** Thin wrapper over InputMethodManager so the keyboard calls stay in one place. */
final class Keyboard {

    private Keyboard() {}

    static void show(Context context, View target) {
        InputMethodManager imm =
            (InputMethodManager) context.getSystemService(Context.INPUT_METHOD_SERVICE);
        if (imm == null) return;
        target.requestFocus();
        imm.showSoftInput(target, InputMethodManager.SHOW_IMPLICIT);
    }

    static void hide(Context context, View target) {
        InputMethodManager imm =
            (InputMethodManager) context.getSystemService(Context.INPUT_METHOD_SERVICE);
        if (imm == null) return;
        imm.hideSoftInputFromWindow(target.getWindowToken(), 0);
    }

    /** True when the target already owns the input session, i.e. the IME is up for it. */
    static boolean isVisible(Context context, View target) {
        InputMethodManager imm =
            (InputMethodManager) context.getSystemService(Context.INPUT_METHOD_SERVICE);
        return imm != null && imm.isActive(target);
    }

    /**
     * Flip the IME for a target that may have never been focused.
     *
     * <p>Returns the state the IME was moved to. The show path has to request focus
     * first: {@code showSoftInput} is a no-op on an unfocused view, which is exactly why
     * the keyboard never appeared when it was only called from {@code startSession()}
     * before the first layout.
     */
    static boolean toggle(Context context, View target) {
        if (isVisible(context, target)) {
            hide(context, target);
            return false;
        }
        show(context, target);
        return true;
    }
}
