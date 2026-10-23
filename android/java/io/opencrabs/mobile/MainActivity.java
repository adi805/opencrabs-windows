package io.opencrabs.mobile;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Color;
import android.graphics.Typeface;
import android.os.Build;
import android.os.Bundle;
import android.util.Log;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;

import com.termux.terminal.TerminalSession;

import java.io.File;

/**
 * Launcher entry point: a full-screen terminal running the OpenCrabs TUI.
 *
 * <p>The core binary is not a library, it is a Rust program that wants a real terminal:
 * it draws with escape sequences and reads raw keys. So the app runs it as a child under a
 * pty and renders the pty output itself, which is what makes the TUI usable on a phone
 * instead of a bare status line.
 *
 * <p>Spawn path: {@link TerminalSession} allocates the pty through
 * {@code com.termux.terminal.JNI}, whose bodies forward to {@link Pty}
 * ({@code forkpty} in {@code android/jni/pty.c}) and start the reader, writer and waiter
 * threads. The pane drives {@code updateSize()}, and the first size is what triggers the
 * actual spawn.
 *
 * <p>Only ONE core per profile may run: a TUI and a daemon on the same profile fight over
 * the channel listeners and the sqlite database. This activity owns the TUI, so it starts
 * {@link CoreService} in TUI mode, where the service holds the foreground notification
 * without spawning its own daemon.
 */
public class MainActivity extends Activity {

    private static final String TAG = "OpenCrabsApp";
    private static final int REQ_POST_NOTIFICATIONS = 1;

    private TerminalPane pane;
    private CoreSessionClient client;
    private TerminalSession session;
    private TextView title;

    /**
     * What a configuration change must not destroy.
     *
     * <p>Rotating the phone tears the activity down and builds a new one. The pty, the
     * child process and its three threads belong to the session, not to the view, so a
     * naive onCreate would leave the old core running and spawn a second one on the same
     * profile: two processes on one sqlite database and one set of channel listeners.
     * Handing the session to the replacement activity keeps exactly one core alive.
     *
     * <p>{@link CoreSessionClient} is retained too because it counts redraws and holds the
     * digest flag; a fresh one would log "tui rendered" a second time and reset the shell
     * pid. It is safe to keep across activities: it stores the application context.
     */
    private static final class Retained {
        final TerminalSession session;
        final CoreSessionClient client;

        Retained(TerminalSession session, CoreSessionClient client) {
            this.session = session;
            this.client = client;
        }
    }

    @Override
    public Object onRetainNonConfigurationInstance() {
        return new Retained(session, client);
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        askForNotificationPermission();

        // The automation bridge is started with the activity but lives on its
        // own daemon thread, so it keeps answering while the app is
        // backgrounded. It refuses to serve anything except /status until the
        // user enables the accessibility service.
        AutomationServer.start(this);

        Retained retained = (Retained) getLastNonConfigurationInstance();
        if (retained != null) {
            client = retained.client;
            session = retained.session;
        } else {
            client = new CoreSessionClient(this);
        }
        pane = new TerminalPane(this);
        client.attachPane(pane);

        title = new TextView(this);
        title.setText("OpenCrabs");
        title.setTextColor(0xffe6e6e6);
        title.setTypeface(Typeface.MONOSPACE);
        title.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        title.setGravity(Gravity.CENTER_VERTICAL);
        title.setPadding(dp(8), dp(4), dp(8), dp(4));

        TextView restart = new TextView(this);
        restart.setText("RESTART");
        restart.setTextColor(0xff7fd1ff);
        restart.setTypeface(Typeface.MONOSPACE);
        restart.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        restart.setGravity(Gravity.CENTER);
        restart.setPadding(dp(10), dp(6), dp(10), dp(6));
        restart.setOnClickListener(v -> restartCore());

        TextView settings = new TextView(this);
        settings.setText("SETTINGS");
        settings.setTextColor(0xff7fd1ff);
        settings.setTypeface(Typeface.MONOSPACE);
        settings.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        settings.setGravity(Gravity.CENTER);
        settings.setPadding(dp(10), dp(6), dp(10), dp(6));
        settings.setOnClickListener(v ->
            startActivity(new Intent(this, SettingsActivity.class)));

        LinearLayout toolbar = new LinearLayout(this);
        toolbar.setOrientation(LinearLayout.HORIZONTAL);
        toolbar.setBackgroundColor(0xff141414);
        LinearLayout.LayoutParams titleParams = new LinearLayout.LayoutParams(
            0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f);
        toolbar.addView(title, titleParams);
        toolbar.addView(settings, new LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        toolbar.addView(restart, new LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setBackgroundColor(Color.BLACK);
        root.addView(toolbar, new LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        root.addView(pane, new LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f));
        root.addView(pane.createExtraKeysRow(), new LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));

        // The pane is full-screen, so a floating control is the only thing that stays
        // reachable while the keyboard is either up or down. It toggles the IME instead
        // of only hiding it, because the extra-keys HIDE key had no way back.
        FrameLayout shell = new FrameLayout(this);
        shell.addView(root, new FrameLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        shell.addView(createKeyboardToggle(), keyboardToggleParams());
        setContentView(shell);

        if (session != null && session.isRunning()) {
            // Same core as before the rotation: point the new pane at it and let the
            // emulator's existing screen state draw. No new process, no new pty.
            Log.i(TAG, "reattached to running session, pid=" + session.getPid());
            pane.attachSession(session);
            pane.requestFocus();
            // Posted, not called inline: showSoftInput on a view that has not been laid
            // out yet is silently ignored, which is why the keyboard never came up on
            // launch. After the first traversal the view has a window token.
            pane.post(() -> Keyboard.show(this, pane));
        } else {
            startSession();
        }
        startForegroundServiceInTuiMode();
    }

    private int dp(int value) {
        return (int) (value * getResources().getDisplayMetrics().density);
    }

    /**
     * Floating keyboard switch, drawn on top of the terminal at the bottom-right.
     *
     * <p>Kept as a plain {@link TextView} rather than a material FAB because this build
     * has no androidx: the APK is assembled with javac + d8 from vendored sources, so a
     * support-library widget would mean unpacking an AAR at build time.
     */
    private View createKeyboardToggle() {
        TextView toggle = new TextView(this);
        // Plain ASCII on purpose: U+2328 (the keyboard glyph) is absent from Roboto on
        // several Android builds and would render as a tofu box.
        toggle.setText("KBD");
        toggle.setTextSize(TypedValue.COMPLEX_UNIT_SP, 20);
        toggle.setTextColor(0xff7fd1ff);
        toggle.setGravity(Gravity.CENTER);
        toggle.setPadding(dp(12), dp(8), dp(12), dp(8));
        toggle.setBackgroundColor(0xcc1f1f1f);
        toggle.setAlpha(0.85f);
        toggle.setOnClickListener(v -> {
            boolean shown = pane.toggleKeyboard();
            v.setAlpha(shown ? 0.45f : 0.85f);
        });
        return toggle;
    }

    /** Sits above the extra-keys row so the two never overlap. */
    private FrameLayout.LayoutParams keyboardToggleParams() {
        FrameLayout.LayoutParams params = new FrameLayout.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT,
            Gravity.BOTTOM | Gravity.END);
        int margin = dp(6);
        params.setMargins(margin, margin, margin, dp(56));
        return params;
    }

    /** Build the environment the core expects, then let the pane's first layout spawn it. */
    private void startSession() {
        File binary = CoreService.coreBinary(this);
        if (!binary.exists()) {
            String message = "core binary missing: " + binary.getAbsolutePath();
            Log.e(TAG, message);
            title.setText(message);
            Toast.makeText(this, message, Toast.LENGTH_LONG).show();
            return;
        }
        if (!binary.canExecute()) {
            Log.w(TAG, "binary is not executable: " + binary.getAbsolutePath());
        }

        File home = new File(getFilesDir(), "home");
        File opencrabsHome = new File(home, ".opencrabs");
        //noinspection ResultOfMethodCallIgnored
        opencrabsHome.mkdirs();

        String[] env = new String[]{
            "HOME=" + home.getAbsolutePath(),
            "OPENCRABS_HOME=" + opencrabsHome.getAbsolutePath(),
            "LD_LIBRARY_PATH=" + CoreService.nativeDir(this),
            "TERM=xterm-256color",
            "COLORTERM=truecolor",
            "PATH=/system/bin:/system/xbin:/vendor/bin",
            "TMPDIR=" + getCacheDir().getAbsolutePath(),
        };

        session = new TerminalSession(
            binary.getAbsolutePath(),
            home.getAbsolutePath(),
            new String[]{"chat"},
            env,
            4000,
            client);
        pane.attachSession(session);
        pane.requestFocus();
        pane.post(() -> Keyboard.show(this, pane));
        Log.i(TAG, "session prepared: " + binary.getAbsolutePath() + " chat cwd=" + home);
    }

    private void restartCore() {
        if (session != null) {
            session.finishIfRunning();
            session = null;
        }
        // onRetainNonConfigurationInstance reads this field, so a later rotation
        // retains whichever session startSession() puts here, never the dead one.
        pane.attachSession(null);
        startSession();
    }

    /**
     * Keep the process alive and show the ongoing notification without letting the service
     * spawn a second core: the TUI in this activity already owns the profile.
     */
    private void startForegroundServiceInTuiMode() {
        Intent service = new Intent(this, CoreService.class);
        service.putExtra(CoreService.EXTRA_TUI_MODE, true);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            startForegroundService(service);
        } else {
            startService(service);
        }
    }

    /**
     * Android 13 (API 33) turned POST_NOTIFICATIONS into a runtime permission.
     *
     * <p>Declaring it in the manifest is not enough. An app that never asks
     * shows no notification at all, so the foreground service that keeps the
     * core alive is invisible: the operator starts it and sees no indicator
     * that it is running, and no way to stop it from the shade. The permission
     * string is a compile-time constant, so referencing it is safe on releases
     * where the permission does not exist yet.
     */
    private void askForNotificationPermission() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
            Log.i(TAG, "POST_NOTIFICATIONS not required on sdk=" + Build.VERSION.SDK_INT);
            return;
        }
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                == PackageManager.PERMISSION_GRANTED) {
            Log.i(TAG, "POST_NOTIFICATIONS already granted");
            return;
        }
        Log.i(TAG, "POST_NOTIFICATIONS requesting (sdk=" + Build.VERSION.SDK_INT + ")");
        requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS},
                REQ_POST_NOTIFICATIONS);
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions,
                                           int[] grantResults) {
        if (requestCode != REQ_POST_NOTIFICATIONS) {
            return;
        }
        boolean granted = grantResults.length > 0
                && grantResults[0] == PackageManager.PERMISSION_GRANTED;
        Log.i(TAG, "POST_NOTIFICATIONS result granted=" + granted);
        if (!granted) {
            return;
        }
        // The service was started before the grant existed, so its foreground
        // notification was suppressed. Re-issuing the start runs
        // startForeground again (now permitted).
        startForegroundServiceInTuiMode();
    }
}
