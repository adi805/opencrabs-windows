package io.opencrabs.mobile;

import android.app.Activity;
import android.content.Intent;
import android.os.Build;
import android.os.Bundle;
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

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

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
}
