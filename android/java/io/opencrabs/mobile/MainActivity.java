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

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        askForNotificationPermission();

        client = new CoreSessionClient(this);
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
        setContentView(root);

        startSession();
        startForegroundServiceInTuiMode();
    }

    private int dp(int value) {
        return (int) (value * getResources().getDisplayMetrics().density);
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
        Keyboard.show(this, pane);
        Log.i(TAG, "session prepared: " + binary.getAbsolutePath() + " chat cwd=" + home);
    }

    private void restartCore() {
        if (session != null) {
            session.finishIfRunning();
            session = null;
        }
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
