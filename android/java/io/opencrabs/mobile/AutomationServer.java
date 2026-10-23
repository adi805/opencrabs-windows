package io.opencrabs.mobile;

import android.content.Context;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.util.List;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * Loopback HTTP surface the agent drives from inside the TUI.
 *
 * <p>Why HTTP instead of a broadcast or a file queue: the agent is a native
 * process sitting on a pty with no Java bridge and no Binder access, but it does
 * have {@code curl}. A loopback socket is the one transport both sides already
 * speak, and it costs nothing while idle because the accept loop blocks.
 *
 * <p>Three deliberate security choices:
 * <ul>
 *   <li>Bound to 127.0.0.1 only. Never 0.0.0.0: an accessibility bridge reachable
 *       from the LAN is remote control of the whole phone.</li>
 *   <li>Every request needs a token, compared in constant time. The token is
 *       generated per install into the app's private files dir, which only this
 *       app and the core process it spawned can read.</li>
 *   <li>Refuses to serve at all while the accessibility service is disconnected,
 *       so a disabled service cannot be half-driven.</li>
 * </ul>
 */
public final class AutomationServer {

    private static final String TAG = "OpenCrabsAuto";

    /** Not 18792: that port belongs to the core's own session surface. */
    public static final int PORT = 18793;

    private static final String TOKEN_FILE = "automation.token";

    private static volatile AutomationServer sInstance;

    private final Context mContext;
    private final ExecutorService mWorkers = Executors.newFixedThreadPool(2);
    private volatile boolean mRunning;
    private ServerSocket mSocket;
    private String mToken;

    private AutomationServer(Context context) {
        mContext = context.getApplicationContext();
    }

    public static synchronized void start(Context context) {
        if (sInstance != null) {
            return;
        }
        AutomationServer s = new AutomationServer(context);
        s.startInternal();
        sInstance = s;
    }

    public static synchronized void stop() {
        if (sInstance == null) {
            return;
        }
        sInstance.stopInternal();
        sInstance = null;
    }

    public static boolean isRunning() {
        return sInstance != null;
    }

    private void startInternal() {
        mToken = loadOrCreateToken();
        Thread t = new Thread(this::acceptLoop, "crabs-automation");
        t.setDaemon(true);
        t.start();
        mRunning = true;
        Log.i(TAG, "automation surface listening on 127.0.0.1:" + PORT);
    }

    private void stopInternal() {
        mRunning = false;
        try {
            if (mSocket != null) {
                mSocket.close();
            }
        } catch (IOException ignored) {
            // closing a socket that is already gone is the desired end state
        }
        mWorkers.shutdownNow();
    }

    private void acceptLoop() {
        try {
            // Loopback only. The second argument caps the backlog so a flood
            // cannot queue unbounded connections.
            mSocket = new ServerSocket(PORT, 8, InetAddress.getByName("127.0.0.1"));
        } catch (IOException e) {
            Log.e(TAG, "cannot bind 127.0.0.1:" + PORT, e);
            mRunning = false;
            return;
        }
        while (mRunning) {
            try {
                final Socket client = mSocket.accept();
                mWorkers.execute(() -> handle(client));
            } catch (IOException e) {
                if (mRunning) {
                    Log.w(TAG, "accept failed", e);
                }
                break;
            }
        }
    }

    private void handle(Socket client) {
        try (Socket c = client) {
            c.setSoTimeout(10000);
            InputStream in = c.getInputStream();

            // Headers are read as raw bytes up to the blank line, then the body
            // is read as exactly Content-Length BYTES. Reading the body through
            // a Reader would decode chars and desynchronise from the byte count
            // the moment a request carries non-ASCII text, which is the normal
            // case here: the agent types Indonesian and emoji into other apps.
            String head = readHeaderBlock(in);
            if (head == null) {
                return;
            }
            String[] lines = head.split("\r\n");
            if (lines.length == 0) {
                writeJson(c, 400, error("bad request line"));
                return;
            }
            String[] parts = lines[0].split(" ");
            if (parts.length < 2) {
                writeJson(c, 400, error("bad request line"));
                return;
            }
            String method = parts[0];
            String path = parts[1];

            int contentLength = 0;
            String token = null;
            for (int i = 1; i < lines.length; i++) {
                int colon = lines[i].indexOf(':');
                if (colon <= 0) {
                    continue;
                }
                String name = lines[i].substring(0, colon).trim().toLowerCase();
                String value = lines[i].substring(colon + 1).trim();
                if ("content-length".equals(name)) {
                    try {
                        contentLength = Integer.parseInt(value);
                    } catch (NumberFormatException e) {
                        contentLength = 0;
                    }
                } else if ("x-crabs-token".equals(name)) {
                    token = value;
                }
            }

            if (!tokenMatches(token)) {
                writeJson(c, 403, error("bad token"));
                return;
            }

            // Cap the body: this surface only ever receives small JSON commands,
            // so a large Content-Length is a mistake or an attempt to exhaust
            // memory. 1 MiB is far above any legitimate command.
            if (contentLength < 0 || contentLength > (1 << 20)) {
                writeJson(c, 413, error("body too large"));
                return;
            }

            byte[] body = new byte[contentLength];
            int read = 0;
            while (read < contentLength) {
                int n = in.read(body, read, contentLength - read);
                if (n < 0) {
                    break;
                }
                read += n;
            }

            route(c, method, path, new String(body, 0, read, StandardCharsets.UTF_8));
        } catch (Exception e) {
            Log.w(TAG, "request failed", e);
        }
    }

    /**
     * Reads bytes until the CRLFCRLF that ends the header block.
     *
     * <p>Returns null when the peer closes before finishing the block, which is
     * a probe or a dropped connection rather than a request worth answering.
     */
    private static String readHeaderBlock(InputStream in) throws IOException {
        java.io.ByteArrayOutputStream buf = new java.io.ByteArrayOutputStream(512);
        int state = 0;
        while (buf.size() < 8192) {
            int b = in.read();
            if (b < 0) {
                return null;
            }
            buf.write(b);
            // 0 = nothing, 1 = CR seen, 2 = CRLF seen, 3 = CRLFCR seen
            if (b == '\r') {
                state = (state == 2) ? 3 : 1;
            } else if (b == '\n') {
                if (state == 1) {
                    state = 2;
                } else if (state == 3) {
                    return buf.toString("UTF-8");
                } else {
                    state = 0;
                }
            } else {
                state = 0;
            }
        }
        return null;
    }

    private void route(Socket c, String method, String path, String body) throws Exception {
        CrabsAccessibilityService svc = CrabsAccessibilityService.getInstance();

        // /status answers even with the service off, so the caller can tell
        // "not enabled" apart from "server down".
        if ("/status".equals(path)) {
            JSONObject o = new JSONObject();
            o.put("server", true);
            o.put("accessibility", CrabsAccessibilityService.isConnected());
            writeJson(c, 200, o);
            return;
        }

        if (svc == null) {
            writeJson(c, 503, error("accessibility service not enabled"));
            return;
        }

        switch (path) {
            case "/packages": {
                JSONArray arr = new JSONArray();
                List<String> pkgs = svc.launchablePackages();
                for (String p : pkgs) {
                    arr.put(p);
                }
                JSONObject o = new JSONObject();
                o.put("packages", arr);
                writeJson(c, 200, o);
                return;
            }
            case "/dump": {
                JSONArray nodes = svc.dumpScreen();
                JSONObject o = new JSONObject();
                o.put("nodes", nodes);
                o.put("count", nodes.length());
                writeJson(c, 200, o);
                return;
            }
            case "/tap": {
                JSONObject in = new JSONObject(body);
                boolean ok = svc.tap((float) in.getDouble("x"), (float) in.getDouble("y"));
                writeJson(c, 200, result(ok));
                return;
            }
            case "/click": {
                JSONObject in = new JSONObject(body);
                boolean ok = svc.clickText(in.getString("text"));
                writeJson(c, 200, result(ok));
                return;
            }
            case "/type": {
                JSONObject in = new JSONObject(body);
                boolean ok = svc.setText(in.getString("text"));
                writeJson(c, 200, result(ok));
                return;
            }
            case "/swipe": {
                JSONObject in = new JSONObject(body);
                boolean ok = svc.swipe(
                        (float) in.getDouble("x1"), (float) in.getDouble("y1"),
                        (float) in.getDouble("x2"), (float) in.getDouble("y2"),
                        in.optLong("ms", 300));
                writeJson(c, 200, result(ok));
                return;
            }
            case "/global": {
                JSONObject in = new JSONObject(body);
                boolean ok = svc.global(in.getString("action"));
                writeJson(c, 200, result(ok));
                return;
            }
            case "/launch": {
                JSONObject in = new JSONObject(body);
                boolean ok = svc.launch(in.getString("package"));
                writeJson(c, 200, result(ok));
                return;
            }
            default:
                writeJson(c, 404, error("no such endpoint"));
        }
    }

    private static JSONObject result(boolean ok) throws Exception {
        JSONObject o = new JSONObject();
        o.put("ok", ok);
        return o;
    }

    private static JSONObject error(String message) throws Exception {
        JSONObject o = new JSONObject();
        o.put("ok", false);
        o.put("error", message);
        return o;
    }

    private void writeJson(Socket c, int status, JSONObject payload) throws IOException {
        byte[] bytes = payload.toString().getBytes(StandardCharsets.UTF_8);
        OutputStream out = c.getOutputStream();
        String head = "HTTP/1.1 " + status + " " + reason(status) + "\r\n"
                + "Content-Type: application/json\r\n"
                + "Content-Length: " + bytes.length + "\r\n"
                + "Connection: close\r\n"
                + "\r\n";
        out.write(head.getBytes(StandardCharsets.UTF_8));
        out.write(bytes);
        out.flush();
    }

    private static String reason(int status) {
        switch (status) {
            case 200: return "OK";
            case 400: return "Bad Request";
            case 403: return "Forbidden";
            case 404: return "Not Found";
            case 413: return "Payload Too Large";
            case 503: return "Service Unavailable";
            default:  return "Error";
        }
    }

    // --- token --------------------------------------------------------------

    /**
     * Compares in constant time. A byte-by-byte early return would leak the
     * token prefix through response timing, which matters here because the
     * endpoint is reachable by any process on the device that can open a socket.
     */
    private boolean tokenMatches(String presented) {
        if (presented == null || mToken == null) {
            return false;
        }
        return MessageDigest.isEqual(
                presented.getBytes(StandardCharsets.UTF_8),
                mToken.getBytes(StandardCharsets.UTF_8));
    }

    private String loadOrCreateToken() {
        File f = new File(mContext.getFilesDir(), TOKEN_FILE);
        try {
            if (f.isFile()) {
                byte[] raw = java.nio.file.Files.readAllBytes(f.toPath());
                String existing = new String(raw, StandardCharsets.UTF_8).trim();
                if (existing.length() >= 32) {
                    return existing;
                }
            }
        } catch (Exception e) {
            Log.w(TAG, "cannot read existing token, regenerating", e);
        }

        byte[] buf = new byte[24];
        new SecureRandom().nextBytes(buf);
        StringBuilder sb = new StringBuilder(buf.length * 2);
        for (byte b : buf) {
            sb.append(String.format("%02x", b));
        }
        String token = sb.toString();
        try (FileOutputStream out = new FileOutputStream(f)) {
            out.write(token.getBytes(StandardCharsets.UTF_8));
            out.flush();
        } catch (Exception e) {
            Log.e(TAG, "cannot persist token; endpoint will use an in-memory value", e);
        }
        // Private app storage, but tighten it anyway: the core process runs as
        // the same uid, no other app needs to read this.
        //noinspection ResultOfMethodCallIgnored
        f.setReadable(false, false);
        //noinspection ResultOfMethodCallIgnored
        f.setReadable(true, true);
        return token;
    }
}
