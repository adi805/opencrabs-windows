package io.opencrabs.mobile;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.Bundle;
import android.util.Log;
import android.widget.TextView;

import java.io.File;

/**
 * Launcher entry point.
 *
 * <p>Deliberately bare for this milestone: the job is to prove the core binary
 * can be spawned and supervised on-device, not to ship a UI. The real session
 * list and transcript view arrive with the session surface (PRD milestone 3).
 */
public class MainActivity extends Activity {

    private static final String TAG = "OpenCrabsApp";
    private static final int REQ_POST_NOTIFICATIONS = 1;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        askForNotificationPermission();

        File binary = CoreService.coreBinary(this);
        StringBuilder status = new StringBuilder();
        status.append("OpenCrabs core\n\n");
        status.append("binary: ").append(binary.getAbsolutePath()).append('\n');
        status.append("present: ").append(binary.exists()).append('\n');
        status.append("executable: ").append(binary.canExecute()).append('\n');
        status.append("abi: ").append(Build.SUPPORTED_ABIS.length > 0
                ? Build.SUPPORTED_ABIS[0] : "unknown").append('\n');

        TextView view = new TextView(this);
        view.setText(status.toString());
        view.setPadding(48, 48, 48, 48);
        setContentView(view);

        Intent service = new Intent(this, CoreService.class);
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
        // startForeground again (now permitted); spawnCore() finds the live
        // core and returns early, so this does not spawn a second one.
        Intent again = new Intent(this, CoreService.class);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            startForegroundService(again);
        } else {
            startService(again);
        }
    }
}
