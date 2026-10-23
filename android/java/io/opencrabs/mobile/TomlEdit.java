package io.opencrabs.mobile;

import java.util.ArrayList;
import java.util.List;

/**
 * Minimal, section-aware TOML key writer.
 *
 * <p>Why not just append text: a TOML file with the same table header twice fails to
 * parse, and a config the core cannot parse means the core does not start. So every write
 * here locates the one header for the target section and edits a key inside it, and only
 * creates the section when no header exists. Everything outside the touched lines is
 * preserved byte for byte, which matters because the user (or the core) may have put
 * comments and other sections in the same file.
 *
 * <p>Scope is deliberately small: flat keys with string values, which is all the settings
 * screen writes. It is not a general TOML editor.
 */
final class TomlEdit {

    private TomlEdit() {}

    /** Quote a value as a TOML basic string. */
    static String quote(String raw) {
        StringBuilder out = new StringBuilder(raw.length() + 2);
        out.append('"');
        for (int i = 0; i < raw.length(); i++) {
            char c = raw.charAt(i);
            switch (c) {
                case '\\':
                    out.append("\\\\");
                    break;
                case '"':
                    out.append("\\\"");
                    break;
                case '\n':
                    out.append("\\n");
                    break;
                case '\r':
                    out.append("\\r");
                    break;
                case '\t':
                    out.append("\\t");
                    break;
                default:
                    out.append(c);
            }
        }
        out.append('"');
        return out.toString();
    }

    /**
     * Set {@code key = value} inside {@code [section]}, creating the section if needed.
     *
     * @param text    current file contents, may be empty
     * @param section header without brackets, e.g. {@code providers.custom.myprovider}
     * @param key     flat key name
     * @param value   already rendered TOML value, e.g. {@code "sk-..."}
     */
    static String setKey(String text, String section, String key, String value) {
        List<String> lines = new ArrayList<>();
        if (text != null && text.length() > 0) {
            for (String line : text.split("\n", -1)) lines.add(line);
        }
        String header = "[" + section + "]";
        String assignment = key + " = " + value;

        int headerIndex = -1;
        for (int i = 0; i < lines.size(); i++) {
            if (stripComment(lines.get(i)).trim().equals(header)) {
                headerIndex = i;
                break;
            }
        }

        if (headerIndex < 0) {
            if (!lines.isEmpty() && !lines.get(lines.size() - 1).trim().isEmpty()) {
                lines.add("");
            }
            lines.add(header);
            lines.add(assignment);
            lines.add("");
            return join(lines);
        }

        int sectionEnd = lines.size();
        for (int i = headerIndex + 1; i < lines.size(); i++) {
            // Only a real table header ends the section. Testing startsWith("[")
            // would also treat a multi-line array value as a header.
            if (isTableHeader(lines.get(i))) {
                sectionEnd = i;
                break;
            }
        }

        for (int i = headerIndex + 1; i < sectionEnd; i++) {
            String trimmed = lines.get(i).trim();
            if (trimmed.startsWith(key) && trimmed.length() > key.length()
                && (trimmed.charAt(key.length()) == '=' || trimmed.charAt(key.length()) == ' ')) {
                lines.set(i, assignment);
                return join(lines);
            }
        }

        lines.add(headerIndex + 1, assignment);
        return join(lines);
    }

    /**
     * Drop a trailing {@code #} comment, respecting quoted strings.
     *
     * <p>Without this, a header written as {@code [agent] # defaults} is not
     * recognised, so saving appends a second {@code [agent]} table and the
     * whole file stops parsing. A '#' inside a quoted value is not a comment.
     */
    private static String stripComment(String line) {
        boolean inString = false;
        for (int i = 0; i < line.length(); i++) {
            char c = line.charAt(i);
            if (c == '"' && (i == 0 || line.charAt(i - 1) != '\\')) {
                inString = !inString;
            } else if (c == '#' && !inString) {
                return line.substring(0, i);
            }
        }
        return line;
    }

    /** True for a line that is exactly one table header, optionally commented. */
    private static boolean isTableHeader(String line) {
        String bare = stripComment(line).trim();
        if (bare.length() < 2 || bare.charAt(0) != '[') return false;
        int close = bare.lastIndexOf(']');
        return close == bare.length() - 1 && bare.indexOf('[') == 0;
    }

    private static String join(List<String> lines) {
        StringBuilder out = new StringBuilder();
        for (int i = 0; i < lines.size(); i++) {
            if (i > 0) out.append('\n');
            out.append(lines.get(i));
        }
        return out.toString();
    }
}
