/*
 * Pseudoterminal support for the OpenCrabs Android shell.
 *
 * Written from scratch against the POSIX pty API that Bionic exports. It is
 * NOT a copy of termux-app's jni/termux.c: that file ships under GPLv3 (the
 * app module), while the two Java modules we vendor from the same project are
 * Apache 2.0. Reimplementing the ~200 lines here keeps the whole APK free of a
 * copyleft obligation that would otherwise attach to the binary we ship.
 *
 * Why a pty at all: the core's TUI is a crossterm/ratatui application. It puts
 * the terminal into raw mode, asks for the window size and addresses the screen
 * with escape sequences. A plain pipe cannot do any of that. forkpty() gives
 * the child a controlling terminal, which is what makes `opencrabs chat`
 * believe it is talking to a real terminal.
 *
 * Bionic provides forkpty() (declared in <pty.h>, available since API 23; this
 * app's floor is API 26), so the hand-rolled openpty()+fork()+setsid()+ioctl
 * dance is not needed. That sequence exists to work around platforms where
 * forkpty is missing or buggy; neither applies here.
 */

#include <jni.h>

#include <errno.h>
#include <fcntl.h>
#include <pty.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

/* Fallback PATH for the child. The app has no shell profile to inherit from,
 * and an empty PATH makes any tool the core tries to spawn fail with ENOENT in
 * a way that looks like a missing binary rather than a missing search path. */
static const char *kDefaultPath = "/system/bin:/system/xbin:/vendor/bin";

/* Build a NULL-terminated char** out of a Java String[]. Returns NULL when the
 * array is absent (which the caller treats as "no arguments"). */
static char **to_c_string_array(JNIEnv *env, jobjectArray array, int *out_count) {
    *out_count = 0;
    if (array == NULL) {
        return NULL;
    }
    jsize len = (*env)->GetArrayLength(env, array);
    if (len <= 0) {
        return NULL;
    }
    char **result = (char **) calloc((size_t) len + 1, sizeof(char *));
    if (result == NULL) {
        return NULL;
    }
    for (jsize i = 0; i < len; i++) {
        jstring item = (jstring) (*env)->GetObjectArrayElement(env, array, i);
        if (item == NULL) {
            result[i] = NULL;
            continue;
        }
        const char *utf = (*env)->GetStringUTFChars(env, item, NULL);
        result[i] = utf == NULL ? NULL : strdup(utf);
        if (utf != NULL) {
            (*env)->ReleaseStringUTFChars(env, item, utf);
        }
        (*env)->DeleteLocalRef(env, item);
    }
    *out_count = (int) len;
    return result;
}

static void free_c_string_array(char **array, int count) {
    if (array == NULL) {
        return;
    }
    for (int i = 0; i < count; i++) {
        free(array[i]);
    }
    free(array);
}

/* Copy a Java string into a freshly allocated C string, or NULL. */
static char *dup_jstring(JNIEnv *env, jstring value) {
    if (value == NULL) {
        return NULL;
    }
    const char *utf = (*env)->GetStringUTFChars(env, value, NULL);
    if (utf == NULL) {
        return NULL;
    }
    char *copy = strdup(utf);
    (*env)->ReleaseStringUTFChars(env, value, utf);
    return copy;
}

JNIEXPORT jint JNICALL
Java_io_opencrabs_mobile_Pty_createSubprocess(JNIEnv *env, jclass clazz,
                                              jstring cmd, jstring cwd,
                                              jobjectArray args, jobjectArray envVars,
                                              jintArray processId, jint rows, jint columns,
                                              jint cellWidth, jint cellHeight) {
    (void) clazz;
    (void) cellWidth;
    (void) cellHeight;

    char *cmd_c = dup_jstring(env, cmd);
    char *cwd_c = dup_jstring(env, cwd);
    int arg_count = 0;
    char **args_c = to_c_string_array(env, args, &arg_count);
    int env_count = 0;
    char **env_c = to_c_string_array(env, envVars, &env_count);

    if (cmd_c == NULL) {
        free_c_string_array(args_c, arg_count);
        free_c_string_array(env_c, env_count);
        free(cwd_c);
        return -1;
    }

    /* argv[0] is the program path when the caller passed only the subcommand,
     * so the child sees a well formed argv either way. */
    int total = arg_count + 2;
    char **argv = (char **) calloc((size_t) total, sizeof(char *));
    if (argv == NULL) {
        free(cmd_c);
        free(cwd_c);
        free_c_string_array(args_c, arg_count);
        free_c_string_array(env_c, env_count);
        return -1;
    }
    argv[0] = cmd_c;
    for (int i = 0; i < arg_count; i++) {
        argv[i + 1] = args_c[i];
    }
    argv[total - 1] = NULL;

    struct winsize size;
    memset(&size, 0, sizeof(size));
    size.ws_row = (unsigned short) (rows > 0 ? rows : 24);
    size.ws_col = (unsigned short) (columns > 0 ? columns : 80);
    size.ws_xpixel = 0;
    size.ws_ypixel = 0;

    int master_fd = -1;
    pid_t pid = forkpty(&master_fd, NULL, NULL, &size);
    if (pid < 0) {
        free(cmd_c);
        free(cwd_c);
        free_c_string_array(env_c, env_count);
        free(argv);
        return -1;
    }

    if (pid == 0) {
        /* Child. forkpty already put us in a new session with the slave as
         * stdin/stdout/stderr and as the controlling terminal. */
        if (cwd_c != NULL) {
            if (chdir(cwd_c) != 0) {
                /* Not fatal: the child can still run from the default cwd. */
            }
        }
        for (int i = 0; i < env_count; i++) {
            if (env_c[i] == NULL) {
                continue;
            }
            char *eq = strchr(env_c[i], '=');
            if (eq == NULL) {
                continue;
            }
            *eq = '\0';
            setenv(env_c[i], eq + 1, 1);
            *eq = '=';
        }
        /* TERM has to be set or crossterm refuses to emit escape sequences at
         * all: without it the TUI degrades to plain text and the renderer shows
         * an empty screen. */
        setenv("TERM", "xterm-256color", 1);
        if (getenv("PATH") == NULL) {
            setenv("PATH", kDefaultPath, 1);
        }
        execv(argv[0], argv);
        /* execv only returns on failure. Report through the pty so the Java
         * side sees a message instead of a silently dead terminal. */
        fprintf(stderr, "exec failed for %s: %s\r\n", argv[0], strerror(errno));
        _exit(127);
    }

    /* Parent. */
    if (processId != NULL) {
        jint pid_value = (jint) pid;
        (*env)->SetIntArrayRegion(env, processId, 0, 1, &pid_value);
    }
    /* argv[] borrows args_c entries; env_c and cwd_c are no longer needed. */
    free(cwd_c);
    free_c_string_array(env_c, env_count);
    free(args_c);
    free(argv);
    return (jint) master_fd;
}

JNIEXPORT void JNICALL
Java_io_opencrabs_mobile_Pty_setPtyWindowSize(JNIEnv *env, jclass clazz, jint fd,
                                              jint rows, jint columns, jint cellWidth,
                                              jint cellHeight) {
    (void) env;
    (void) clazz;
    (void) cellWidth;
    (void) cellHeight;
    struct winsize size;
    memset(&size, 0, sizeof(size));
    size.ws_row = (unsigned short) rows;
    size.ws_col = (unsigned short) columns;
    ioctl((int) fd, TIOCSWINSZ, &size);
}

JNIEXPORT jint JNICALL
Java_io_opencrabs_mobile_Pty_waitFor(JNIEnv *env, jclass clazz, jint pid) {
    (void) env;
    (void) clazz;
    int status = 0;
    pid_t result;
    do {
        result = waitpid((pid_t) pid, &status, 0);
    } while (result == -1 && errno == EINTR);

    if (result == -1) {
        return -1;
    }
    if (WIFEXITED(status)) {
        return WEXITSTATUS(status);
    }
    if (WIFSIGNALED(status)) {
        /* Negative of the signal, matching the shell convention the Java side
         * documents. */
        return -WTERMSIG(status);
    }
    return -1;
}

JNIEXPORT void JNICALL
Java_io_opencrabs_mobile_Pty_close(JNIEnv *env, jclass clazz, jint fd) {
    (void) env;
    (void) clazz;
    if (fd >= 0) {
        close((int) fd);
    }
}
