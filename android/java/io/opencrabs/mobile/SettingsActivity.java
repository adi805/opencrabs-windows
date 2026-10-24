package io.opencrabs.mobile;

import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.Intent;
import android.graphics.Typeface;
import android.net.Uri;
import android.os.Bundle;
import android.os.PowerManager;
import android.text.InputType;
import android.util.Log;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.Switch;
import android.widget.TextView;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;

/**
 * Provider setup screen: the smallest thing that stops the TUI dead-ending on onboarding.
 *
 * <p>The core reads {@code config.toml} and {@code keys.toml} from its own home. A freshly
 * installed app has neither, so the first thing the TUI does is ask for a provider and a
 * key, in a full-screen dialog that a phone keyboard makes miserable. This screen writes
 * both files directly, so the TUI starts on a working configuration.
 *
 * <p>Writes are section-aware (see {@link TomlEdit}): a duplicate table header makes the
 * file unparseable, and a config the core cannot parse means the core does not start.
 * Nothing outside the touched keys is rewritten, so comments and unrelated sections survive.
 */
public class SettingsActivity extends Activity {

    private static final String TAG = "OpenCrabsSettings";

    private EditText providerField;
    private EditText baseUrlField;
    private EditText modelField;
    private EditText keyField;
    private TextView status;
    private Switch bootSwitch;
    private Button batteryButton;
    private TextView batteryStatus;
    private Button automationButton;
    private TextView automationStatus;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        int pad = dp(16);
        root.setPadding(pad, pad, pad, pad);

        TextView heading = new TextView(this);
        heading.setText("Provider");
        heading.setTypeface(Typeface.DEFAULT_BOLD);
        heading.setTextSize(TypedValue.COMPLEX_UNIT_SP, 18);
        root.addView(heading);

        TextView hint = new TextView(this);
        hint.setText("Written to config.toml and keys.toml in the app's OpenCrabs home. "
            + "Leave base URL empty for a built-in provider; fill it for a custom "
            + "OpenAI-compatible endpoint.");
        hint.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        hint.setPadding(0, dp(6), 0, dp(12));
        root.addView(hint);

        providerField = addField(root, "provider", "opencrabs",
            InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        baseUrlField = addField(root, "base URL (optional)", "",
            InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI);
        modelField = addField(root, "model (optional)", "",
            InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        keyField = addField(root, "API key", "",
            InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD);

        Button save = new Button(this);
        save.setText("Save");
        save.setOnClickListener(v -> save());
        LinearLayout.LayoutParams saveParams = new LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        saveParams.topMargin = dp(12);
        saveParams.gravity = Gravity.END;
        root.addView(save, saveParams);

        status = new TextView(this);
        status.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        status.setTypeface(Typeface.MONOSPACE);
        status.setPadding(0, dp(12), 0, 0);
        root.addView(status);

        addBackgroundSection(root);
        addAutomationSection(root);

        // Scrolled: the background section pushed the content past a small
        // phone's viewport, and a LinearLayout alone would clip the last control
        // with no way to reach it.
        ScrollView scroll = new ScrollView(this);
        scroll.addView(root);
        setContentView(scroll);
        loadCurrent();
    }

    /**
     * Background survival controls.
     *
     * <p>Both of these are things the app cannot grant itself: one is a user
     * preference, the other is a system dialog. Presenting them here is the only
     * honest way to offer them, because silently holding a wakelock or
     * resurrecting on every boot is what malware does.
     */
    private void addBackgroundSection(LinearLayout root) {
        TextView heading = new TextView(this);
        heading.setText("Background");
        heading.setTypeface(Typeface.DEFAULT_BOLD);
        heading.setTextSize(TypedValue.COMPLEX_UNIT_SP, 18);
        heading.setPadding(0, dp(24), 0, 0);
        root.addView(heading);

        TextView hint = new TextView(this);
        hint.setText("The core keeps running as a foreground service while the app is "
            + "in the background. These two settings make it survive a reboot and stop "
            + "Android from deferring its work.");
        hint.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        hint.setPadding(0, dp(6), 0, dp(8));
        root.addView(hint);

        bootSwitch = new Switch(this);
        bootSwitch.setText("Start on boot");
        bootSwitch.setChecked(CoreService.startOnBoot(this));
        bootSwitch.setOnCheckedChangeListener((v, checked) ->
            CoreService.setStartOnBoot(SettingsActivity.this, checked));
        root.addView(bootSwitch);

        batteryButton = new Button(this);
        batteryButton.setOnClickListener(v -> requestBatteryExemption());
        root.addView(batteryButton);

        batteryStatus = new TextView(this);
        batteryStatus.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        batteryStatus.setPadding(0, dp(8), 0, 0);
        root.addView(batteryStatus);
    }

    /**
     * Asks the system for a Doze exemption.
     *
     * <p>A foreground service already keeps its network access during Doze
     * (power-details: foreground service process gets "No restrictions" on
     * network), so this is not needed for the agent to reach a provider. It is
     * needed for anything time-based, because Doze defers alarms and jobs
     * regardless of foreground state. The dialog is the system's, so the user
     * grants it explicitly; the app cannot grant it to itself.
     */
    private void requestBatteryExemption() {
        PowerManager pm = (PowerManager) getSystemService(POWER_SERVICE);
        if (pm != null && pm.isIgnoringBatteryOptimizations(getPackageName())) {
            batteryStatus.setText("Already exempt from battery optimization.");
            return;
        }
        try {
            Intent intent = new Intent(
                android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS);
            intent.setData(Uri.parse("package:" + getPackageName()));
            startActivity(intent);
        } catch (ActivityNotFoundException e) {
            // Some OEM builds ship without this activity. Falling back to the
            // list of all apps still lets the user find us, which beats a crash.
            try {
                startActivity(new Intent(
                    android.provider.Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS));
            } catch (ActivityNotFoundException ignored) {
                batteryStatus.setText("This build has no battery optimization screen.");
            }
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        // Re-read on resume: the exemption is granted in a different app
        // (Settings), so the label would otherwise keep claiming "not exempt"
        // after the user just granted it.
        if (batteryButton != null) {
            PowerManager pm = (PowerManager) getSystemService(POWER_SERVICE);
            boolean exempt = pm != null && pm.isIgnoringBatteryOptimizations(getPackageName());
            batteryButton.setText(exempt
                ? "Battery optimization: unrestricted"
                : "Allow unrestricted battery use");
            batteryButton.setEnabled(!exempt);
            if (exempt) {
                batteryStatus.setText("Exempt. Doze will not defer this app's alarms.");
            }
        }
        refreshAutomationStatus();
    }

    /**
     * The bridge that lets the agent drive other apps.
     *
     * <p>This is the one control in this screen that hands over real power: with
     * it on, the agent can read the screen of any app and tap anything. So the
     * switch here is not a switch at all, it is a button that opens the system's
     * own Accessibility screen. Android deliberately does not let an app enable
     * its own accessibility service, and routing through the system UI is the
     * only way the grant stays something the user did rather than something the
     * app did to them.
     */
    private void addAutomationSection(LinearLayout root) {
        TextView heading = new TextView(this);
        heading.setText("Automation");
        heading.setTypeface(Typeface.DEFAULT_BOLD);
        heading.setTextSize(TypedValue.COMPLEX_UNIT_SP, 18);
        heading.setPadding(0, dp(24), 0, 0);
        root.addView(heading);

        TextView hint = new TextView(this);
        hint.setText("Lets the agent read other apps' screens and tap, type or swipe "
            + "for you. Off unless you turn it on in Android's accessibility settings, "
            + "and Android shows a permanent switch there to revoke it. Automating a "
            + "third-party service can violate its terms and put your account at risk, "
            + "so use it on apps that permit it.");
        hint.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        hint.setPadding(0, dp(6), 0, dp(8));
        root.addView(hint);

        automationButton = new Button(this);
        automationButton.setOnClickListener(v -> openAccessibilitySettings());
        root.addView(automationButton);

        automationStatus = new TextView(this);
        automationStatus.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        automationStatus.setTypeface(Typeface.MONOSPACE);
        automationStatus.setPadding(0, dp(8), 0, 0);
        root.addView(automationStatus);
    }

    private void openAccessibilitySettings() {
        try {
            startActivity(new Intent(android.provider.Settings.ACTION_ACCESSIBILITY_SETTINGS));
        } catch (ActivityNotFoundException e) {
            automationStatus.setText("This build has no accessibility settings screen.");
        }
    }

    private void refreshAutomationStatus() {
        if (automationButton == null || automationStatus == null) {
            return;
        }
        boolean connected = CrabsAccessibilityService.isConnected();
        automationButton.setText(connected
            ? "Automation is on (tap to review)"
            : "Enable automation in accessibility settings");
        automationStatus.setText(connected
            ? "bridge ready on 127.0.0.1:" + AutomationServer.PORT
            : "bridge idle: service not enabled");
    }

    private EditText addField(LinearLayout root, String label, String initial, int inputType) {
        TextView caption = new TextView(this);
        caption.setText(label);
        caption.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        caption.setPadding(0, dp(8), 0, 0);
        root.addView(caption);

        EditText field = new EditText(this);
        field.setText(initial);
        field.setInputType(inputType);
        field.setSingleLine(true);
        field.setTypeface(Typeface.MONOSPACE);
        root.addView(field, new LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        return field;
    }

    private int dp(int value) {
        return (int) (value * getResources().getDisplayMetrics().density);
    }

    /** The OpenCrabs home the core is launched with (MainActivity passes the same path). */
    private File opencrabsHome() {
        return new File(new File(getFilesDir(), "home"), ".opencrabs");
    }

    private void loadCurrent() {
        String config = readText(new File(opencrabsHome(), "config.toml"));
        if (config == null) return;
        String provider = firstValue(config, "agent", "default_provider");
        if (provider != null && provider.length() > 0) {
            providerField.setText(provider);
            // The base URL decides which table save() writes to. Left blank, a
            // custom provider is re-saved as [providers.<name>] while the core
            // still reads [providers.custom.<name>], so the screen reports
            // success and the old key stays in force.
            String baseUrl = firstValue(config, "providers.custom." + provider, "base_url");
            if (baseUrl != null && baseUrl.length() > 0) {
                baseUrlField.setText(baseUrl);
            }
        }
        String model = firstValue(config, "agent", "default_model");
        if (model != null && model.length() > 0) {
            modelField.setText(model);
        }
    }

    /**
     * Pull the first {@code key = "value"} out of the text. Deliberately not a TOML
     * parser: this only pre-fills the form so the user sees what is already configured.
     */
    private static String firstValue(String text, String section, String key) {
        // Scoped to one section on purpose: "default_model" appears both in
        // [agent] and in every [providers.<name>], and reading the provider's
        // copy would pre-fill the form with a value that then gets written back
        // as the agent default.
        boolean inSection = false;
        for (String line : text.split("\n")) {
            String trimmed = line.trim();
            if (trimmed.startsWith("[")) {
                inSection = trimmed.equals("[" + section + "]");
                continue;
            }
            if (!inSection) continue;
            if (!trimmed.startsWith(key)) continue;
            int eq = trimmed.indexOf('=');
            if (eq < 0) continue;
            String value = trimmed.substring(eq + 1).trim();
            if (value.startsWith("\"") && value.endsWith("\"") && value.length() >= 2) {
                value = value.substring(1, value.length() - 1);
            }
            return value;
        }
        return null;
    }

    private void save() {
        String provider = providerField.getText().toString().trim();
        String baseUrl = baseUrlField.getText().toString().trim();
        String model = modelField.getText().toString().trim();
        String key = keyField.getText().toString().trim();

        if (provider.length() == 0) {
            status.setText("provider is required");
            return;
        }
        // The name is spliced into a table header ([providers.custom.<name>]),
        // so a space or a quote produces a file the core cannot parse, which
        // means the core does not start. Reject here rather than write it.
        if (!provider.matches("[A-Za-z0-9_-]+")) {
            status.setText("provider name: letters, digits, dash and underscore only");
            return;
        }
        // The built-in provider is keyless on a fresh install (the core seeds its
        // own install token), so an empty key is only fatal for every other
        // provider, where the user is the only possible source of the secret.
        boolean builtIn = "opencrabs".equals(provider);
        if (key.length() == 0 && !builtIn) {
            status.setText("API key is required for " + provider);
            return;
        }
        if (baseUrl.length() > 0 && !baseUrl.startsWith("http")) {
            status.setText("base URL must start with http");
            return;
        }

        File home = opencrabsHome();
        if (!home.isDirectory() && !home.mkdirs()) {
            status.setText("cannot create " + home.getAbsolutePath());
            return;
        }

        boolean custom = baseUrl.length() > 0;
        String providerSection = custom ? "providers.custom." + provider : "providers." + provider;

        StringBuilder report = new StringBuilder();

        File configFile = new File(home, "config.toml");
        String config = readText(configFile);
        if (config == null) config = "";
        config = TomlEdit.setKey(config, providerSection, "enabled", "true");
        if (custom) {
            config = TomlEdit.setKey(config, providerSection, "base_url", TomlEdit.quote(baseUrl));
        }
        if (model.length() > 0) {
            config = TomlEdit.setKey(config, providerSection, "default_model",
                TomlEdit.quote(model));
        }
        config = TomlEdit.setKey(config, "agent", "default_provider", TomlEdit.quote(provider));
        if (model.length() > 0) {
            config = TomlEdit.setKey(config, "agent", "default_model", TomlEdit.quote(model));
        }
        if (!writeText(configFile, config)) {
            status.setText("cannot write config.toml");
            return;
        }
        report.append("config.toml: [").append(providerSection).append("]\n");

        File keysFile = new File(home, "keys.toml");
        String keys = readText(keysFile);
        if (keys == null) keys = "";
        if (key.length() > 0) {
            keys = TomlEdit.setKey(keys, providerSection, "api_key", TomlEdit.quote(key));
            if (!writeText(keysFile, keys)) {
                status.setText("cannot write keys.toml");
                return;
            }
            report.append("keys.toml: [").append(providerSection).append("] api_key\n");
        } else {
            report.append("keys.toml: unchanged (built-in provider needs no key)\n");
        }
        report.append("restart the terminal to apply");

        Log.i(TAG, "saved provider=" + provider + " custom=" + custom + " home=" + home);
        status.setText(report.toString());
        keyField.setText("");
    }

    private static String readText(File file) {
        if (!file.isFile()) return null;
        FileInputStream in = null;
        try {
            in = new FileInputStream(file);
            byte[] buffer = new byte[(int) file.length()];
            int offset = 0;
            while (offset < buffer.length) {
                int read = in.read(buffer, offset, buffer.length - offset);
                if (read < 0) break;
                offset += read;
            }
            return new String(buffer, 0, offset, "UTF-8");
        } catch (IOException e) {
            Log.e(TAG, "read failed: " + file, e);
            return null;
        } finally {
            if (in != null) {
                try {
                    in.close();
                } catch (IOException ignored) {
                    // Nothing to do: the read already returned or failed.
                }
            }
        }
    }

    private static boolean writeText(File file, String content) {
        FileOutputStream out = null;
        try {
            out = new FileOutputStream(file);
            out.write(content.getBytes("UTF-8"));
            return true;
        } catch (IOException e) {
            Log.e(TAG, "write failed: " + file, e);
            return false;
        } finally {
            if (out != null) {
                try {
                    out.close();
                } catch (IOException ignored) {
                    // Same as above: the write result is already decided.
                }
            }
        }
    }
}
