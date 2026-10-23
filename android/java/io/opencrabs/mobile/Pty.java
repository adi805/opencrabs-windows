package io.opencrabs.mobile;

/**
 * Native pseudoterminal binding.
 *
 * <p>The implementation lives in {@code android/jni/pty.c} and is built into
 * {@code libopencrabspty.so}. It is loaded by name, which means the loader looks
 * for it in the app's native library directory: the same place the core binary
 * is staged, and the only directory Android allows an executable image to be
 * mapped from since W^X was enforced on the app data directory (API 29).
 *
 * <p>This class is the only path to a terminal. Without it the core would run
 * with pipes for stdin/stdout, crossterm would see {@code isatty() == false},
 * and {@code opencrabs chat} would either refuse to start its TUI or paint
 * escape sequences into a log file. A pty is not a nicety here, it is the
 * precondition for the TUI existing at all.
 */
final class Pty {

    static {
        System.loadLibrary("opencrabspty");
    }

    private Pty() {
        // Static holder: nothing to instantiate.
    }

    /**
     * Fork a child with a pseudoterminal as its controlling terminal.
     *
     * @param cmd       absolute path of the executable to run
     * @param cwd       working directory for the child, or null to inherit
     * @param args      arguments after argv[0], or null for none
     * @param envVars   entries of the form {@code KEY=value}, applied on top of
     *                  the inherited environment
     * @param processId a one-element array that receives the child's pid
     * @param rows      initial terminal rows
     * @param columns   initial terminal columns
     * @param cellWidth pixel width of one cell, or 0 when unknown
     * @param cellHeight pixel height of one cell, or 0 when unknown
     * @return the master side file descriptor, or -1 on failure
     */
    public static native int createSubprocess(String cmd, String cwd, String[] args,
                                       String[] envVars, int[] processId,
                                       int rows, int columns,
                                       int cellWidth, int cellHeight);

    /**
     * Tell the child's terminal how large it is.
     *
     * <p>Called on every layout change. A TUI that never receives this keeps
     * drawing for the size it was started with, so a rotation or a keyboard
     * opening leaves the bottom of the screen stale.
     */
    public static native void setPtyWindowSize(int fd, int rows, int columns,
                                        int cellWidth, int cellHeight);

    /**
     * Block until the child exits.
     *
     * @return the exit status when it exited normally, the negated signal when
     *         it was killed, or -1 when the wait itself failed
     */
    public static native int waitFor(int processId);

    /** Close a file descriptor through {@code close(2)}. */
    public static native void close(int fileDescriptor);
}
