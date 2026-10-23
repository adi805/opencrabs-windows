package io.opencrabs.mobile;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.os.Build;
import android.util.Log;

/**
 * Brings the core back after a reboot, and only when the operator asked for it.
 *
 * <p>Two things make this receiver necessary and one thing makes it optional:
 *
 * <p>Necessary: {@code START_STICKY} survives the platform killing the service
 * under memory pressure, but a reboot is not a kill. Every process is gone and
 * nothing is scheduled to bring ours back, so without this the agent is simply
 * absent until the user opens the app.
 *
 * <p>Necessary: {@code MY_PACKAGE_REPLACED} covers an in-place upgrade. The
 * package manager kills the app during install, and on some OEM builds the
 * service is not restarted afterwards even though it is sticky.
 *
 * <p>Optional: the whole thing is gated on {@link CoreService#PREF_START_ON_BOOT},
 * which defaults to false. Auto-starting a long-running process that the user
 * did not ask for is exactly the behaviour users (rightly) read as malware, and
 * it costs battery on every boot even for someone who opens the app once a week.
 * The Settings screen owns that switch.
 *
 * <p>The service is started in daemon mode (no TUI extra). At boot there is no
 * activity and no pty, so a TUI-mode start would hold the foreground
 * notification while spawning nothing, which is worse than not starting at all.
 * Daemon mode is also the safe choice for the database: it is the mode that does
 * not collide with a terminal that might be opened later, because
 * {@code CoreService} remembers the mode and a subsequent explicit TUI start
 * overrides it.
 */
public class BootReceiver extends BroadcastReceiver {

    private static final String TAG = "OpenCrabsBoot";

    @Override
    public void onReceive(Context context, Intent intent) {
        String action = intent == null ? null : intent.getAction();
        if (action == null) {
            return;
        }
        // Explicit allowlist rather than "anything that reaches us": a receiver
        // with an intent-filter is reachable by any broadcast whose action
        // matches, and acting on an action we did not reason about is how a
        // receiver becomes a free start-service primitive for other apps.
        if (!Intent.ACTION_BOOT_COMPLETED.equals(action)
                && !Intent.ACTION_MY_PACKAGE_REPLACED.equals(action)) {
            Log.i(TAG, "ignoring action " + action);
            return;
        }
        if (!CoreService.startOnBoot(context)) {
            Log.i(TAG, action + ": start-on-boot is off, not starting the core");
            return;
        }
        Log.i(TAG, action + ": starting the core in daemon mode");

        Intent service = new Intent(context, CoreService.class);
        // Daemon mode, explicitly. Leaving the extra out would make the service
        // fall back to the remembered mode, and the remembered mode can be TUI,
        // which spawns nothing at boot.
        service.putExtra(CoreService.EXTRA_TUI_MODE, false);
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.startForegroundService(service);
            } else {
                context.startService(service);
            }
        } catch (IllegalStateException e) {
            // Android 12+ throws when a background start is not allowed. At boot
            // this is normally permitted, but OEM builds and some battery savers
            // deny it. Nothing to retry: the user can open the app, and the log
            // line is the only honest signal we can leave.
            Log.w(TAG, "background start refused: " + e.getMessage());
        }
    }
}
