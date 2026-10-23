package com.termux.terminal;

import io.opencrabs.mobile.Pty;

/**
 * Native methods for creating and managing pseudoterminal subprocesses.
 *
 * <p>MODIFIED for OpenCrabs: upstream this class is a direct
 * {@code System.loadLibrary("termux")} binding into {@code jni/termux.c}, which
 * lives in termux-app's GPLv3 app module. This APK cannot carry that: the whole
 * point of vendoring the terminal-emulator is that it is Apache 2.0. So the
 * signatures are kept identical, which is what lets every other vendored file
 * compile unchanged, and the bodies forward to {@code io.opencrabs.mobile.Pty},
 * our own forkpty implementation built as {@code libopencrabspty.so}.
 *
 * <p>Keeping the signatures is not laziness: {@link TerminalSession} calls these
 * four methods and nothing else, so an adapter here is a far smaller diff than
 * editing the session class and risking a divergence from upstream that a later
 * re-vendor would silently undo.
 */
final class JNI {

    private JNI() {
        // Static adapter, nothing to instantiate.
    }

    /**
     * Create a subprocess. Differs from {@link ProcessBuilder} in that a pseudoterminal is used to
     * communicate with the subprocess.
     * <p/>
     * Callers are responsible for calling {@link #close(int)} on the returned file descriptor.
     *
     * @param cmd       The command to execute
     * @param cwd       The current working directory for the executed command
     * @param args      An array of arguments to the command
     * @param envVars   An array of strings of the form "VAR=value" to be added to the environment of
     *                  the process
     * @param processId A one-element array to which the process ID of the started process will be
     *                  written.
     * @return the file descriptor resulting from opening /dev/ptmx master device. The sub process
     * will have opened the slave device counterpart (/dev/pts/$N) and will have it as stdint,
     * stdout and stderr.
     */
    public static int createSubprocess(String cmd, String cwd, String[] args,
                                       String[] envVars, int[] processId, int rows, int columns,
                                       int cellWidth, int cellHeight) {
        return Pty.createSubprocess(cmd, cwd, args, envVars, processId,
                rows, columns, cellWidth, cellHeight);
    }

    /** Set the window size for a given pty, which allows connected programs to learn how large
     * their screen is. */
    public static void setPtyWindowSize(int fd, int rows, int cols, int cellWidth, int cellHeight) {
        Pty.setPtyWindowSize(fd, rows, cols, cellWidth, cellHeight);
    }

    /**
     * Causes the calling thread to wait for the process associated with the receiver to finish
     * executing.
     *
     * @return if >= 0, the exit status of the process. If < 0, the signal causing the process to
     * stop negated.
     */
    public static int waitFor(int processId) {
        return Pty.waitFor(processId);
    }

    /** Close a file descriptor through the close(2) system call. */
    public static void close(int fileDescriptor) {
        Pty.close(fileDescriptor);
    }

}
