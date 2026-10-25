package io.opencrabs.mobile;

import android.Manifest;
import android.annotation.TargetApi;
import android.app.Activity;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.graphics.Color;
import android.graphics.Insets;
import android.graphics.Typeface;
import android.os.Build;
import android.os.Bundle;
import android.util.Log;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.ScrollView;
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

    /** UI state that must survive a restart: font size and the FULL toggle. */
    private static final String PREFS = TerminalPane.PREFS;
    private static final String PREF_IMMERSIVE = "immersive";

    private TerminalPane pane;
    private CoreSessionClient client;
    private TerminalSession session;
    private TextView title;
    private LinearLayout toolbar;
    private SharedPreferences prefs;

    /** True while the IME is up, i.e. while the title bar earns its space back. */
    private boolean typing;

    /** User-requested full screen: system bars and title bar hidden. */
    private boolean immersive;

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
        // First statement in the process's first activity: everything after this point is
        // covered by the handler, including the setup below.
        CrashLog.install(this);
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

        prefs = getSharedPreferences(PREFS, MODE_PRIVATE);
        immersive = prefs.getBoolean(PREF_IMMERSIVE, false);
        title = new TextView(this);
        title.setText("OpenCrabs");
        title.setTextColor(0xffe6e6e6);
        title.setTypeface(Typeface.MONOSPACE);
        title.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        title.setGravity(Gravity.CENTER_VERTICAL);
        // 2dp vertical instead of 4: the title bar is chrome, and every dp it keeps is a
        // dp the terminal does not get.
        title.setPadding(dp(8), dp(2), dp(8), dp(2));

        TextView full = toolButton("FULL");
        full.setOnClickListener(v -> setFullScreen(!immersive));

        TextView settings = toolButton("SETTINGS");
        settings.setOnClickListener(v ->
            startActivity(new Intent(this, SettingsActivity.class)));

        TextView restart = toolButton("RESTART");
        restart.setOnClickListener(v -> restartCore());

        toolbar = new LinearLayout(this);
        toolbar.setOrientation(LinearLayout.HORIZONTAL);
        toolbar.setBackgroundColor(0xff141414);
        LinearLayout.LayoutParams titleParams = new LinearLayout.LayoutParams(
            0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f);
        toolbar.addView(title, titleParams);
        toolbar.addView(full, wrap());
        toolbar.addView(settings, wrap());
        toolbar.addView(restart, wrap());

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

        setContentView(shell);

        // Edge-to-edge and the insets listener go in AFTER setContentView. The window
        // insets controller and the decor view only exist once there is content to
        // attach them to; touching them earlier is the difference between a running app
        // and a force close on builds that enforce that ordering.
        applyEdgeToEdge();
        installInsetListener(shell);
        applyImmersive(immersive);
        toolbar.setVisibility(immersive ? View.GONE : View.VISIBLE);
        showPreviousCrash(shell);

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

    /**
     * Render the previous run's stack trace, if it crashed.
     *
     * <p>The build box has no adb path to the phone, so a force close used to be a dead
     * end: the system dialog names the app and nothing else. Showing the trace on the next
     * launch means one screenshot carries the whole diagnosis, and it is dismissed only by
     * the CLEAR button so it cannot disappear before it is read.
     */
    private void showPreviousCrash(FrameLayout shell) {
        String trace = CrashLog.last(this);
        if (trace == null) return;
        Log.e(TAG, "previous run crashed:\n" + trace);

        TextView body = new TextView(this);
        body.setText(trace);
        body.setTextColor(0xffffb4b4);
        body.setTextSize(TypedValue.COMPLEX_UNIT_SP, 9);
        body.setTypeface(Typeface.MONOSPACE);
        body.setPadding(dp(8), dp(8), dp(8), dp(8));

        ScrollView scroll = new ScrollView(this);
        scroll.addView(body);

        LinearLayout panel = new LinearLayout(this);
        panel.setOrientation(LinearLayout.VERTICAL);
        panel.setBackgroundColor(0xff1a0000);
        panel.addView(scroll, new LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f));

        TextView dismiss = toolButton("CLEAR CRASH LOG");
        dismiss.setOnClickListener(v -> {
            CrashLog.clear(this);
            shell.removeView(panel);
        });
        panel.addView(dismiss, wrap());

        shell.addView(panel, new FrameLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
    }

    private int dp(int value) {
        return (int) (value * getResources().getDisplayMetrics().density);
    }

    /** Compact toolbar button: 12sp monospace, 6dp horizontal padding. */
    private TextView toolButton(String label) {
        TextView button = new TextView(this);
        button.setText(label);
        button.setTextColor(0xff7fd1ff);
        button.setTypeface(Typeface.MONOSPACE);
        button.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        button.setGravity(Gravity.CENTER);
        button.setPadding(dp(6), dp(2), dp(6), dp(2));
        return button;
    }

    private static LinearLayout.LayoutParams wrap() {
        return new LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
    }

    /**
     * Stop the window from fitting itself around the system bars.
     *
     * <p>This is the fix for the keyboard covering the terminal. {@code
     * android:windowSoftInputMode="adjustResize"} in the manifest is a no-op for apps
     * targeting API 30 or newer, and this app targets 35, so the window never shrank and
     * the IME was simply drawn on top of the prompt line and the extra-keys row. Once the
     * window stops fitting itself, the insets arrive at {@link #installInsetListener} and
     * the padding there is what actually keeps content above the keyboard.
     *
     * <p>Below API 30 the manifest attribute still works, so the legacy path only records
     * that it did nothing. Splitting the paths keeps every reference to a class that does
     * not exist on API 26 inside a method that is never called there.
     */
    private void applyEdgeToEdge() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            applyEdgeToEdgeModern();
        } else {
            Log.i(TAG, "edge-to-edge skipped: sdk=" + Build.VERSION.SDK_INT
                + ", adjustResize still applies");
        }
    }

    @TargetApi(Build.VERSION_CODES.R)
    private void applyEdgeToEdgeModern() {
        getWindow().setDecorFitsSystemWindows(false);
    }

    private void installInsetListener(View root) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            installInsetListenerModern(root);
        } else {
            installInsetListenerLegacy(root);
        }
    }

    /**
     * Pad the shell by the bars and the IME, whichever reaches further.
     *
     * <p>max() and not a sum: with the keyboard down the IME inset is zero and the
     * navigation bar is the constraint, with it up the IME already covers that bar, so
     * adding them would leave a dead strip of black under the extra-keys row.
     *
     * <p>The same pass is the single source of truth for "the user is typing", which is
     * what collapses the title bar. Deriving it from the inset instead of tracking IME
     * show/hide calls means a swipe-down keyboard, a hardware keyboard, or a rotation
     * cannot leave the bar in the wrong state.
     */
    @TargetApi(Build.VERSION_CODES.R)
    private void installInsetListenerModern(View root) {
        root.setOnApplyWindowInsetsListener((v, insets) -> {
            Insets bars = insets.getInsets(WindowInsets.Type.systemBars());
            Insets ime = insets.getInsets(WindowInsets.Type.ime());
            v.setPadding(0, bars.top, 0, Math.max(bars.bottom, ime.bottom));
            setTyping(ime.bottom > 0);
            return insets;
        });
    }

    @SuppressWarnings("deprecation")
    private void installInsetListenerLegacy(View root) {
        root.setOnApplyWindowInsetsListener((v, insets) -> {
            v.setPadding(0, insets.getSystemWindowInsetTop(), 0,
                insets.getSystemWindowInsetBottom());
            setTyping(insets.getSystemWindowInsetBottom() > dp(60));
            return insets;
        });
    }

    /**
     * Collapse the title bar while the keyboard is up.
     *
     * <p>On a 360dp-wide phone the bar costs about 22dp plus the status bar, and while
     * typing that is the difference between seeing the last few lines of the TUI and not.
     * Guarded by the previous value because this runs from an insets pass: flipping
     * visibility unconditionally would request another layout on every inset delivery.
     */
    private void setTyping(boolean nowTyping) {
        if (nowTyping == typing) return;
        typing = nowTyping;
        // Immersive already hides the bar, so it owns visibility while it is on.
        if (!immersive) toolbar.setVisibility(typing ? View.GONE : View.VISIBLE);
    }

    private void setFullScreen(boolean on) {
        immersive = on;
        prefs.edit().putBoolean(PREF_IMMERSIVE, on).apply();
        applyImmersive(on);
        toolbar.setVisibility(on || typing ? View.GONE : View.VISIBLE);
        Toast.makeText(this, on ? "FULL: swipe to reveal bars" : "FULL off",
            Toast.LENGTH_SHORT).show();
    }

    private void applyImmersive(boolean on) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            applyImmersiveModern(on);
        } else {
            applyImmersiveLegacy(on);
        }
    }

    @TargetApi(Build.VERSION_CODES.R)
    private void applyImmersiveModern(boolean on) {
        WindowInsetsController controller = getWindow().getInsetsController();
        if (controller == null) return;
        if (on) {
            controller.hide(WindowInsets.Type.systemBars());
            controller.setSystemBarsBehavior(
                WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
        } else {
            controller.show(WindowInsets.Type.systemBars());
        }
    }

    @SuppressWarnings("deprecation")
    private void applyImmersiveLegacy(boolean on) {
        View decor = getWindow().getDecorView();
        int flags = decor.getSystemUiVisibility();
        if (on) {
            flags |= View.SYSTEM_UI_FLAG_FULLSCREEN | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION;
        } else {
            flags &= ~(View.SYSTEM_UI_FLAG_FULLSCREEN | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION);
        }
        decor.setSystemUiVisibility(flags);
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
    protected void onResume() {
        super.onResume();
        // The automation endpoints that start an activity (/launch, /share) are
        // only honoured by the platform while this app is in front. Track it so
        // the bridge can say "blocked" instead of a green ok that did nothing.
        AutomationServer.sAppForeground = true;
    }

    @Override
    protected void onPause() {
        AutomationServer.sAppForeground = false;
        super.onPause();
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
