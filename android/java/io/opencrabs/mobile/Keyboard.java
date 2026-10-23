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
}
