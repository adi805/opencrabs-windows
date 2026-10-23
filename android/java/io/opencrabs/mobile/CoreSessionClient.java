package io.opencrabs.mobile;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.util.Log;

import java.nio.charset.StandardCharsets;

import com.termux.terminal.TerminalBuffer;
import com.termux.terminal.TerminalEmulator;
import com.termux.terminal.TerminalRow;
import com.termux.terminal.TerminalSession;
import com.termux.terminal.TerminalSessionClient;

/**
 * Glue between a vendored {@link TerminalSession} and our {@link TerminalPane}.
 *
 * <p>The emulator calls these callbacks from the main thread (TerminalSession funnels
 * reader output through a main-thread handler), so redraws can call {@code invalidate()}
 * directly. Log callbacks are routed to logcat so a broken child process leaves a trace
 * instead of failing silently.
 */
final class CoreSessionClient implements TerminalSessionClient {

    private static final String TAG = "OpenCrabsTerm";

    private final Context context;
    private TerminalPane pane;
    private int shellPid = -1;

    /** Count of emulator redraw callbacks, i.e. how often the child wrote to the pty. */
    private int textChangedCount;
    /** Set once the screen has been measured, so the digest below is logged exactly once. */
    private boolean renderDigestLogged;

    CoreSessionClient(Context context) {
        this.context = context.getApplicationContext();
    }

    void attachPane(TerminalPane pane) {
        this.pane = pane;
    }

    int shellPid() {
        return shellPid;
    }

    private void redraw() {
        if (pane != null) pane.invalidate();
    }

    @Override
    public void onTextChanged(TerminalSession changedSession) {
        redraw();
        textChangedCount++;
        logRenderDigestOnce(changedSession);
    }

    /**
     * Emit one log line proving the core actually drew something, for the CI
     * emulator smoke test.
     *
     * <p>Without this, "the app did not crash" is the only thing a headless run
     * can say about the TUI: the pixels live in a Canvas the harness cannot
     * read, and the child's stdout goes into the pty rather than logcat. So the
     * emulator itself measures the rendered screen here, once, and logs the
     * shape of it. A blank screen, an empty pty or a child that exited
     * immediately all produce {@code chars=0}, which is exactly the failure the
     * smoke test needs to catch.
     *
     * <p>Only counts and dimensions are logged, never screen text: this line
     * runs in CI on a throwaway image, but a digest that cannot leak what the
     * operator was reading is the right default.
     */
    private void logRenderDigestOnce(TerminalSession session) {
        if (renderDigestLogged || textChangedCount < 3) return;
        renderDigestLogged = true;
        TerminalEmulator emulator;
        try {
            emulator = session.getEmulator();
        } catch (RuntimeException e) {
            Log.w(TAG, "render digest unavailable: " + e);
            return;
        }
        TerminalBuffer screen = emulator.getScreen();
        String text = screen.getTranscriptText();
        int chars = text == null ? 0 : text.length();
        int rows = emulator.getRows();
        int columns = emulator.getColumns();
        int nonEmptyRows = 0;
        for (int row = 0; row < rows; row++) {
            TerminalRow line = screen.getRowForRendering(row);
            if (line != null && line.getSpaceUsed() > 0) nonEmptyRows++;
        }
        Log.i(TAG, "tui rendered: textChanged=" + textChangedCount
            + " chars=" + chars
            + " nonEmptyRows=" + nonEmptyRows
            + " rows=" + rows
            + " cols=" + columns);
    }

    @Override
    public void onTitleChanged(TerminalSession changedSession) {
        Log.i(TAG, "title=" + changedSession.getTitle());
    }

    @Override
    public void onSessionFinished(TerminalSession finishedSession) {
        Log.i(TAG, "session finished, exit=" + finishedSession.getExitStatus());
        redraw();
    }

    @Override
    public void onCopyTextToClipboard(TerminalSession session, String text) {
        ClipboardManager manager =
            (ClipboardManager) context.getSystemService(Context.CLIPBOARD_SERVICE);
        if (manager != null && text != null) {
            manager.setPrimaryClip(ClipData.newPlainText("terminal", text));
        }
    }

    @Override
    public void onPasteTextFromClipboard(TerminalSession session) {
        ClipboardManager manager =
            (ClipboardManager) context.getSystemService(Context.CLIPBOARD_SERVICE);
        if (manager == null || !manager.hasPrimaryClip()) return;
        ClipData clip = manager.getPrimaryClip();
        if (clip == null || clip.getItemCount() == 0) return;
        CharSequence text = clip.getItemAt(0).coerceToText(context);
        if (text == null || text.length() == 0) return;
        byte[] bytes = text.toString().getBytes(StandardCharsets.UTF_8);
        session.write(bytes, 0, bytes.length);
    }

    @Override
    public void onBell(TerminalSession session) {
        Log.d(TAG, "bell");
    }

    @Override
    public void onColorsChanged(TerminalSession session) {
        redraw();
    }

    @Override
    public void onTerminalCursorStateChange(boolean state) {
        redraw();
    }

    @Override
    public void setTerminalShellPid(TerminalSession session, int pid) {
        shellPid = pid;
        Log.i(TAG, "shell pid=" + pid);
    }

    @Override
    public Integer getTerminalCursorStyle() {
        return null;
    }

    @Override
    public void logError(String tag, String message) {
        Log.e(TAG, tag + ": " + message);
    }

    @Override
    public void logWarn(String tag, String message) {
        Log.w(TAG, tag + ": " + message);
    }

    @Override
    public void logInfo(String tag, String message) {
        Log.i(TAG, tag + ": " + message);
    }

    @Override
    public void logDebug(String tag, String message) {
        Log.d(TAG, tag + ": " + message);
    }

    @Override
    public void logVerbose(String tag, String message) {
        Log.v(TAG, tag + ": " + message);
    }

    @Override
    public void logStackTraceWithMessage(String tag, String message, Exception e) {
        Log.e(TAG, tag + ": " + message, e);
    }

    @Override
    public void logStackTrace(String tag, Exception e) {
        Log.e(TAG, tag, e);
    }
}
