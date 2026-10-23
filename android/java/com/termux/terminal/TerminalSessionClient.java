package com.termux.terminal;

/**
 * The interface for communication between {@link TerminalSession} and its client. It is used to
 * send callbacks to the client when {@link TerminalSession} changes or for sending other
 * back data to the client like logs.
 *
 * <p>MODIFIED for OpenCrabs: the upstream copy annotates parameters with
 * {@code NonNull} and {@code Nullable}. Those annotations are
 * the only non-platform annotation reference in the whole terminal-emulator module, and this
 * APK is assembled by a hand-rolled javac + d8 script with no Gradle and no AAR
 * resolution, so adding a Gradle-resolved annotation jar just for two compile-time hints would mean
 * teaching the build to unpack an AAR. The annotations carry no runtime
 * behaviour (CLASS retention), so dropping them changes nothing observable.
 * See NOTICE in this directory for the license and the full list of changes.
 */
public interface TerminalSessionClient {

    void onTextChanged(TerminalSession changedSession);

    void onTitleChanged(TerminalSession changedSession);

    void onSessionFinished(TerminalSession finishedSession);

    void onCopyTextToClipboard(TerminalSession session, String text);

    void onPasteTextFromClipboard(TerminalSession session);

    void onBell(TerminalSession session);

    void onColorsChanged(TerminalSession session);

    void onTerminalCursorStateChange(boolean state);

    void setTerminalShellPid(TerminalSession session, int pid);

    Integer getTerminalCursorStyle();

    void logError(String tag, String message);

    void logWarn(String tag, String message);

    void logInfo(String tag, String message);

    void logDebug(String tag, String message);

    void logVerbose(String tag, String message);

    void logStackTraceWithMessage(String tag, String message, Exception e);

    void logStackTrace(String tag, Exception e);

}
