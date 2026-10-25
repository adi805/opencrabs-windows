package io.opencrabs.mobile;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.graphics.RectF;
import android.graphics.Typeface;
import android.os.Handler;
import android.os.Looper;
import android.text.InputType;
import android.util.Log;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.View;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;

import com.termux.terminal.KeyHandler;
import com.termux.terminal.TerminalBuffer;
import com.termux.terminal.TerminalColors;
import com.termux.terminal.TerminalEmulator;
import com.termux.terminal.TerminalRow;
import com.termux.terminal.TerminalSession;
import com.termux.terminal.TextStyle;
import com.termux.terminal.WcWidth;

import java.nio.charset.StandardCharsets;

/**
 * Canvas renderer + input front end for a {@link TerminalSession}.
 *
 * <p>This is a hand-written replacement for Termux's terminal-view module, which we do not
 * vendor because it drags in androidx and needs an AAR unpacked at build time. Only the
 * terminal-emulator module (Apache 2.0) is vendored; everything below is drawn from the
 * public emulator API: {@link TerminalBuffer#getRowForRendering}, {@link TerminalRow#mText},
 * {@link TextStyle} decoders and {@link KeyHandler}.
 *
 * <p>Input path: soft keyboard text goes through {@link BaseInputConnection#commitText},
 * hardware keys through {@link #onKeyDown} which maps them with {@link KeyHandler} to the
 * escape sequences a TUI expects. Both end in {@link TerminalSession#write}.
 */
public final class TerminalPane extends View {

    private static final String TAG = "OpenCrabsTerm";

    /** Set by the extra-keys row so an armed modifier that gets consumed stops glowing. */
    private Runnable modifierListener;

    /** UI state that must survive a restart, shared with the activity's own toggles. */
    static final String PREFS = "opencrabs.ui";
    static final String PREF_TEXT_SIZE = "text_px";

    /** Cell sizes the A-/A+ keys step through, in raw pixels. */
    private static final int TEXT_PX_MIN = 18;
    private static final int TEXT_PX_MAX = 64;
    private static final int TEXT_PX_STEP = 3;

    private static final int CURSOR_BLINK_MS = 500;

    /** View tag on the extra-keys row, so the activity can find or hide it without a field. */
    public static final String EXTRA_KEYS = "opencrabs.extraKeys";

    private TerminalSession session;
    private int textSizePx = 30;
    private int cellWidth;
    private int cellHeight;
    /** Last size pushed to the pty, so a layout pass does not re-issue the same resize. */
    private int lastCols = -1;
    private int lastRows = -1;

    private final Paint textPaint = new Paint();
    private final Paint fillPaint = new Paint();
    private final RectF tmpRect = new RectF();
    private final StringBuilder runText = new StringBuilder(256);
    /** Scratch buffer for the per-cell fallback draw, one glyph cluster at a time. */
    private final StringBuilder cellText = new StringBuilder(8);

    private boolean ctrlArmed;
    private boolean altArmed;
    private boolean cursorVisible = true;

    private final Handler blinkHandler = new Handler(Looper.getMainLooper());
    private final Runnable blink = new Runnable() {
        @Override
        public void run() {
            cursorVisible = !cursorVisible;
            invalidate();
            blinkHandler.postDelayed(this, CURSOR_BLINK_MS);
        }
    };

    public TerminalPane(Context context) {
        super(context);
        setFocusable(true);
        setFocusableInTouchMode(true);
        setClickable(true);
        setBackgroundColor(0xff000000);

        textPaint.setTypeface(Typeface.MONOSPACE);
        textPaint.setAntiAlias(true);
        textPaint.setTextSize(textSizePx);
        fillPaint.setStyle(Paint.Style.FILL);
        setTextSizePx(loadTextSizePx());
    }

    /** Attach the session this pane renders. Call after {@link TerminalSession#initializeEmulator}. */
    public void attachSession(TerminalSession session) {
        this.session = session;
        updateSessionSize();
        requestLayout();
        invalidate();
    }

    public TerminalSession getSession() {
        return session;
    }

    /**
     * Build the on-screen key row for this pane (ESC, TAB, CTRL, ALT, arrows, ENTER, PASTE).
     * Tagged {@link #EXTRA_KEYS} so the activity can locate it without holding a field.
     */
    public View createExtraKeysRow() {
        View row = ExtraKeys.create(getContext(), this);
        row.setTag(EXTRA_KEYS);
        return row;
    }

    public void setTextSizePx(int px) {
        textSizePx = Math.max(12, px);
        textPaint.setTextSize(textSizePx);
        Paint.FontMetrics fm = textPaint.getFontMetrics();
        cellHeight = (int) Math.ceil(fm.descent - fm.ascent + fm.leading);
        cellWidth = (int) Math.ceil(textPaint.measureText("M"));
        if (cellWidth <= 0) cellWidth = textSizePx / 2;
        requestLayout();
        updateSessionSize();
        invalidate();
    }

    int getTextSizePx() {
        return textSizePx;
    }

    /**
     * Step the cell size, and remember the choice.
     *
     * <p>Font size is the biggest lever on a phone screen: a smaller cell fits more rows
     * AND more columns, so this is the control that decides how much of the TUI is
     * visible at once. The value is persisted here rather than in the activity because the
     * pane already has a {@link Context}, and a listener hop through the activity bought
     * nothing.
     */
    public void stepTextSize(int delta) {
        int next = Math.max(TEXT_PX_MIN,
            Math.min(TEXT_PX_MAX, textSizePx + delta * TEXT_PX_STEP));
        setTextSizePx(next);
        getContext().getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putInt(PREF_TEXT_SIZE, next).apply();
    }

    private int loadTextSizePx() {
        int stored = getContext().getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getInt(PREF_TEXT_SIZE, 0);
        if (stored <= 0) return textSizePx;
        return Math.max(TEXT_PX_MIN, Math.min(TEXT_PX_MAX, stored));
    }

    @Override
    protected void onSizeChanged(int w, int h, int oldw, int oldh) {
        super.onSizeChanged(w, h, oldw, oldh);
        updateSessionSize();
    }

    private void updateSessionSize() {
        if (session == null || cellWidth <= 0 || cellHeight <= 0) return;
        int cols = Math.max(2, getWidth() / cellWidth);
        int rows = Math.max(2, getHeight() / cellHeight);
        session.updateSize(cols, rows, cellWidth, cellHeight);
    }

    @Override
    protected void onAttachedToWindow() {
        super.onAttachedToWindow();
        startBlink();
    }

    @Override
    protected void onDetachedFromWindow() {
        stopBlink();
        super.onDetachedFromWindow();
    }

    /**
     * Window visibility is the hook that actually fires when the app is
     * backgrounded.
     *
     * <p>This matters for battery, not just for looks. {@code onDetachedFromWindow}
     * only runs when the view is removed from the hierarchy, and pressing Home does
     * not remove it: the activity is stopped, the window becomes invisible, and the
     * view stays attached. The blink loop therefore kept toggling and calling
     * {@code invalidate()} twice a second while the app sat in the background, which
     * is 7200 CPU wakeups per hour for a cursor nobody can see. Measured target for
     * this app is under 2% battery per hour idle, and this loop alone would blow it.
     */
    @Override
    protected void onWindowVisibilityChanged(int visibility) {
        super.onWindowVisibilityChanged(visibility);
        if (visibility == VISIBLE) {
            startBlink();
        } else {
            stopBlink();
        }
    }

    private void startBlink() {
        blinkHandler.removeCallbacks(blink);
        cursorVisible = true;
        blinkHandler.postDelayed(blink, CURSOR_BLINK_MS);
    }

    private void stopBlink() {
        blinkHandler.removeCallbacks(blink);
        // Leave the cursor solid rather than frozen mid-blink, so returning to the
        // app does not show a cursor that is invisible until the next toggle.
        cursorVisible = true;
    }

    // ---------------------------------------------------------------- drawing

    /**
     * Frames actually painted. Kept because "the buffer holds text" and "a frame
     * was drawn" are different claims: a pane that never gets laid out can hold
     * a full screen and still show nothing.
     */
    private int drawCount;

    int drawCount() {
        return drawCount;
    }

    @Override
    protected void onDraw(Canvas canvas) {
        drawCount++;
        canvas.drawColor(0xff000000);
        if (session == null) return;
        TerminalEmulator emulator = session.getEmulator();
        if (emulator == null) return;
        TerminalBuffer screen = emulator.getScreen();
        TerminalColors colors = emulator.mColors;

        int rows = emulator.getRows();
        int cols = emulator.getColumns();
        if (drawCount == 1) {
            Log.i(TAG, "tui frame: draw=1 rows=" + rows + " cols=" + cols
                + " cell=" + cellWidth + "x" + cellHeight);
        }
        int baselineOffset = (int) -textPaint.getFontMetrics().ascent;

        for (int row = 0; row < rows; row++) {
            TerminalRow line = screen.getRowForRendering(row);
            if (line == null) continue;
            int top = row * cellHeight;
            int runStartCol = 0;
            long runStyle = Long.MIN_VALUE;
            runText.setLength(0);
            int lastCharIndex = -1;

            for (int col = 0; col <= cols; col++) {
                long style = (col < cols) ? screen.getStyleAt(row, col) : Long.MIN_VALUE;
                if (col < cols && style == runStyle) {
                    int charIndex = line.findStartOfColumn(col);
                    if (charIndex != lastCharIndex) {
                        lastCharIndex = charIndex;
                        appendCellText(line, charIndex, runText);
                    }
                    continue;
                }
                if (runStartCol < col && runStyle != Long.MIN_VALUE) {
                    drawRun(canvas, colors, line, row, runStartCol, col, runStyle, runText,
                        top, baselineOffset);
                }
                if (col < cols) {
                    runStartCol = col;
                    runStyle = style;
                    runText.setLength(0);
                    lastCharIndex = -1;
                    col--; // re-handle this cell as the first cell of the new run
                }
            }
        }

        drawCursor(canvas, emulator, colors, baselineOffset);
    }

    /**
     * Append a cell's full text: the base code point plus the combining marks
     * stored after it.
     *
     * <p>Combining marks occupy zero columns and live in the same cell as their
     * base, so copying only the first code point dropped them and "e" + U+0301
     * rendered as plain "e". They sit immediately after the base char, which is
     * why the walk stops at the first non-zero-width code point.
     */
    private static void appendCellText(TerminalRow line, int charIndex, StringBuilder out) {
        int codePoint = Character.codePointAt(line.mText, charIndex);
        if (codePoint == 0) return;
        out.appendCodePoint(codePoint);
        int i = charIndex + Character.charCount(codePoint);
        int used = line.getSpaceUsed();
        while (i < used) {
            int next = Character.codePointAt(line.mText, i);
            if (next == 0 || WcWidth.width(next) > 0) break;
            out.appendCodePoint(next);
            i += Character.charCount(next);
        }
    }

    private void drawRun(Canvas canvas, TerminalColors colors, TerminalRow line, int row,
                         int fromCol, int toCol, long style, StringBuilder text, int top,
                         int baselineOffset) {
        int effect = TextStyle.decodeEffect(style);
        boolean inverse = (effect & TextStyle.CHARACTER_ATTRIBUTE_INVERSE) != 0;
        int fore = resolveColor(colors, TextStyle.decodeForeColor(style));
        int back = resolveColor(colors, TextStyle.decodeBackColor(style));
        if (inverse) {
            int swap = fore;
            fore = back;
            back = swap;
        }

        float left = fromCol * cellWidth;
        float right = toCol * cellWidth;
        tmpRect.set(left, top, right, top + cellHeight);
        fillPaint.setColor(back);
        canvas.drawRect(tmpRect, fillPaint);

        boolean invisible = (effect & TextStyle.CHARACTER_ATTRIBUTE_INVISIBLE) != 0;
        if (invisible || text.length() == 0) return;

        textPaint.setColor(fore);
        textPaint.setFakeBoldText((effect & TextStyle.CHARACTER_ATTRIBUTE_BOLD) != 0);
        textPaint.setTextSkewX((effect & TextStyle.CHARACTER_ATTRIBUTE_ITALIC) != 0 ? -0.25f : 0f);
        textPaint.setAlpha((effect & TextStyle.CHARACTER_ATTRIBUTE_DIM) != 0 ? 140 : 255);
        // Draw the run in one call only when it genuinely fits its cell span. Fake bold
        // widens every glyph advance by a fraction of a pixel, and a whole run drawn at
        // natural advances then drifts right: over 80 columns the tail lands past the
        // last cell and the right edge of the line gets clipped. Measuring first and
        // falling back to one drawText per cell keeps every glyph inside its own column.
        float span = (toCol - fromCol) * (float) cellWidth;
        if (textPaint.measureText(text, 0, text.length()) <= span) {
            canvas.drawText(text, 0, text.length(), left, top + baselineOffset, textPaint);
        } else {
            for (int col = fromCol; col < toCol; col++) {
                int charIndex = line.findStartOfColumn(col);
                cellText.setLength(0);
                appendCellText(line, charIndex, cellText);
                if (cellText.length() == 0) continue;
                canvas.drawText(cellText, 0, cellText.length(), col * (float) cellWidth,
                    top + baselineOffset, textPaint);
            }
        }
        textPaint.setFakeBoldText(false);
        textPaint.setTextSkewX(0f);
        textPaint.setAlpha(255);

        float underlineY = top + cellHeight - 2;
        if ((effect & TextStyle.CHARACTER_ATTRIBUTE_UNDERLINE) != 0) {
            fillPaint.setColor(fore);
            canvas.drawRect(left, underlineY, right, underlineY + 2, fillPaint);
        }
        if ((effect & TextStyle.CHARACTER_ATTRIBUTE_STRIKETHROUGH) != 0) {
            float strikeY = top + cellHeight / 2f;
            fillPaint.setColor(fore);
            canvas.drawRect(left, strikeY, right, strikeY + 2, fillPaint);
        }
    }

    private void drawCursor(Canvas canvas, TerminalEmulator emulator, TerminalColors colors, int baselineOffset) {
        if (!emulator.shouldCursorBeVisible() || !cursorVisible) return;
        int row = emulator.getCursorRow();
        int col = emulator.getCursorCol();
        if (row < 0 || col < 0 || row >= emulator.getRows() || col >= emulator.getColumns()) return;
        float left = col * cellWidth;
        float top = row * cellHeight;
        fillPaint.setColor(resolveColor(colors, TextStyle.COLOR_INDEX_CURSOR));
        // The emulator already translated DECSCUSR into its own three values:
        // 0 block, 1 underline, 2 bar. Comparing against the raw 3..6 never
        // matched, so every requested shape came out as a block.
        int cursorStyle = emulator.getCursorStyle();
        if (cursorStyle == TerminalEmulator.TERMINAL_CURSOR_STYLE_UNDERLINE) {
            canvas.drawRect(left, top + cellHeight - 3, left + cellWidth, top + cellHeight, fillPaint);
        } else if (cursorStyle == TerminalEmulator.TERMINAL_CURSOR_STYLE_BAR) {
            canvas.drawRect(left, top, left + 3, top + cellHeight, fillPaint);
        } else {
            canvas.drawRect(left, top, left + cellWidth, top + cellHeight, fillPaint);
            TerminalRow line = emulator.getScreen().getRowForRendering(row);
            if (line != null) {
                long style = emulator.getScreen().getStyleAt(row, col);
                int charIndex = line.findStartOfColumn(col);
                int codePoint = Character.codePointAt(line.mText, charIndex);
                if (codePoint != 0 && codePoint != ' ' && WcWidth.width(codePoint) > 0) {
                    textPaint.setColor(resolveColor(colors, TextStyle.decodeBackColor(style)));
                    canvas.drawText(new String(Character.toChars(codePoint)), left, top + baselineOffset, textPaint);
                }
            }
        }
    }

    private static int resolveColor(TerminalColors colors, int color) {
        if (color >= TextStyle.COLOR_INDEX_FOREGROUND) {
            return colors.mCurrentColors[color];
        }
        if ((color & 0xff000000) == 0xff000000) {
            return color;
        }
        return colors.mCurrentColors[color];
    }

    // ------------------------------------------------------------------ input

    /**
     * A tap on the terminal brings the keyboard up.
     *
     * <p>Without this the pane was a dead rectangle: {@code View} only requests focus on
     * touch, and requesting focus does not raise the IME on its own, so the soft keyboard
     * could never be summoned after {@code Keyboard.hide()} or after the first layout.
     * Focus plus an explicit {@code showSoftInput} is what actually opens it.
     */
    @Override
    public boolean onTouchEvent(MotionEvent event) {
        int action = event.getActionMasked();
        if (action == MotionEvent.ACTION_DOWN) {
            // Claim the gesture so the UP below is delivered to this view.
            return true;
        }
        if (action == MotionEvent.ACTION_UP) {
            requestFocus();
            Keyboard.show(getContext(), this);
            return true;
        }
        return super.onTouchEvent(event);
    }

    /** Flip the soft keyboard for this pane. Returns the state it was moved to. */
    boolean toggleKeyboard() {
        requestFocus();
        return Keyboard.toggle(getContext(), this);
    }

    /** True while the IME is up for this pane, so a toggle button can draw its state. */
    boolean isKeyboardVisible() {
        return Keyboard.isVisible(getContext(), this);
    }

    @Override
    public boolean onCheckIsTextEditor() {
        return true;
    }

    @Override
    public InputConnection onCreateInputConnection(EditorInfo outAttrs) {
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS;
        outAttrs.imeOptions = EditorInfo.IME_ACTION_NONE | EditorInfo.IME_FLAG_NO_FULLSCREEN
            | EditorInfo.IME_FLAG_NO_EXTRACT_UI | EditorInfo.IME_FLAG_NO_ENTER_ACTION;
        return new BaseInputConnection(this, true) {
            @Override
            public boolean commitText(CharSequence text, int newCursorPosition) {
                // Soft keyboards arrive here, never through onKeyDown, so without this
                // the armed CTRL button was decorative: arming CTRL and typing "c"
                // sent a literal "c" instead of ETX.
                sendText(applyModifiers(text));
                return true;
            }

            @Override
            public boolean sendKeyEvent(KeyEvent event) {
                return TerminalPane.this.dispatchKeyEvent(event);
            }

            @Override
            public boolean deleteSurroundingText(int beforeLength, int afterLength) {
                // Soft keyboards implement backspace as a delete, a terminal wants 0x7f.
                for (int i = 0; i < beforeLength; i++) sendBytes(new byte[]{0x7f});
                return true;
            }
        };
    }

    @Override
    public boolean onKeyDown(int keyCode, KeyEvent event) {
        if (session == null) return super.onKeyDown(keyCode, event);
        TerminalEmulator emulator = session.getEmulator();

        if (keyCode == KeyEvent.KEYCODE_BACK && !event.isCtrlPressed() && !altArmed) {
            return super.onKeyDown(keyCode, event);
        }

        int keyMode = 0;
        if (ctrlArmed || event.isCtrlPressed()) keyMode |= KeyHandler.KEYMOD_CTRL;
        if (altArmed || event.isAltPressed()) keyMode |= KeyHandler.KEYMOD_ALT;
        if (event.isShiftPressed()) keyMode |= KeyHandler.KEYMOD_SHIFT;

        String code = KeyHandler.getCode(keyCode, keyMode,
            emulator != null && emulator.isCursorKeysApplicationMode(),
            emulator != null && emulator.isKeypadApplicationMode());

        if (code != null) {
            sendText(code);
            clearModifiers();
            return true;
        }

        int unicode = event.getUnicodeChar(event.getMetaState());
        if (unicode != 0) {
            // KeyHandler only maps the special keys; a plain letter falls through to
            // here, where the modifier flags used to be dropped on the floor.
            sendText(applyModifiers(new String(Character.toChars(unicode))));
            return true;
        }
        return super.onKeyDown(keyCode, event);
    }

    /**
     * Turn typed text into the bytes a terminal expects, given the armed modifiers.
     *
     * <p>CTRL maps a character into the control range the same way a real terminal does:
     * a letter loses its top three bits ({@code c} becomes 0x03) and the {@code @} to
     * {@code _} run maps to 0x00 to 0x1f, with {@code ?} becoming DEL. ALT prefixes ESC.
     * Both modifiers are consumed by the keystroke, and the on-screen buttons are told so
     * they stop claiming to be armed.
     */
    private String applyModifiers(CharSequence text) {
        if (text == null || text.length() == 0) return "";
        boolean ctrl = ctrlArmed;
        boolean alt = altArmed;
        if (!ctrl && !alt) return text.toString();
        StringBuilder out = new StringBuilder(text.length() + 2);
        if (alt) out.append('\u001b');
        if (!ctrl) {
            out.append(text);
        } else {
            for (int i = 0; i < text.length(); i++) {
                char c = text.charAt(i);
                if (c >= 'a' && c <= 'z') {
                    out.append((char) (c - 'a' + 1));
                } else if (c >= 'A' && c <= 'Z') {
                    out.append((char) (c - 'A' + 1));
                } else if (c >= '@' && c <= '_') {
                    out.append((char) (c - '@'));
                } else if (c == ' ') {
                    out.append('\u0000');
                } else if (c == '?') {
                    out.append('\u007f');
                } else {
                    out.append(c);
                }
            }
        }
        clearModifiers();
        return out.toString();
    }

    /**
     * Paste through the emulator, not as keystrokes.
     *
     * <p>The emulator knows whether the application asked for bracketed paste (DECSET
     * 2004) and wraps the text accordingly, which is what keeps a multi-line prompt one
     * editable buffer instead of submitting at the first newline. It also strips embedded
     * escape sequences, so pasting text from a web page cannot drive the terminal.
     */
    public void pasteText(CharSequence text) {
        if (text == null || text.length() == 0 || session == null) return;
        TerminalEmulator emulator = session.getEmulator();
        if (emulator == null) {
            sendText(text);
            return;
        }
        emulator.paste(text.toString());
        cursorVisible = true;
        invalidate();
    }

    /** Notified whenever a sticky modifier changes, so the keys row can redraw. */
    void setModifierListener(Runnable listener) {
        this.modifierListener = listener;
    }

    public void sendText(CharSequence text) {
        if (text == null || text.length() == 0) return;
        sendBytes(text.toString().getBytes(StandardCharsets.UTF_8));
    }

    public void sendBytes(byte[] bytes) {
        if (session == null || bytes == null || bytes.length == 0) return;
        session.write(bytes, 0, bytes.length);
        cursorVisible = true;
        invalidate();
    }

    /** Sticky CTRL for the on-screen extra keys row. */
    public void toggleCtrl() {
        ctrlArmed = !ctrlArmed;
        if (ctrlArmed) altArmed = false;
        notifyModifiers();
    }

    /** Sticky ALT for the on-screen extra keys row. */
    public void toggleAlt() {
        altArmed = !altArmed;
        if (altArmed) ctrlArmed = false;
        notifyModifiers();
    }

    public boolean isCtrlArmed() {
        return ctrlArmed;
    }

    public boolean isAltArmed() {
        return altArmed;
    }

    public void clearModifiers() {
        boolean changed = ctrlArmed || altArmed;
        ctrlArmed = false;
        altArmed = false;
        if (changed) notifyModifiers();
    }

    private void notifyModifiers() {
        if (modifierListener != null) modifierListener.run();
    }
}
