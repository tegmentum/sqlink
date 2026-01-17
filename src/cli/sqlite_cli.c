/*
 * SQLite WASM CLI
 *
 * A command-line interface for SQLite, similar to the native sqlite3 CLI.
 * Runs as a WASI component.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdbool.h>
#include <ctype.h>
#include "sqlite3.h"

/* Maximum input line length */
#define MAX_LINE 8192

/* Maximum number of columns to display */
#define MAX_COLUMNS 100

/* Maximum SQL buffer size */
#define MAX_SQL_BUFFER 65536

/* Output modes */
typedef enum {
    MODE_LIST,      /* Values separated by separator */
    MODE_COLUMN,    /* Columnar output */
    MODE_LINE,      /* One value per line */
    MODE_CSV,       /* CSV format */
    MODE_TABLE,     /* ASCII table */
    MODE_MARKDOWN,  /* Markdown table */
    MODE_JSON,      /* JSON array */
} OutputMode;

/* CLI state */
typedef struct {
    sqlite3 *db;
    char *db_filename;
    OutputMode mode;
    char separator[20];
    bool show_headers;
    bool echo_on;
    bool stats_on;
    bool bail_on_error;
    int changes;
    char *null_display;
    int col_widths[MAX_COLUMNS];
    char prompt[64];
    char continue_prompt[64];
} CliState;

/* Forward declarations */
static void print_help(void);
static void print_version(void);
static int do_meta_command(CliState *state, const char *line);
static int execute_sql(CliState *state, const char *sql);
static void print_row_list(CliState *state, sqlite3_stmt *stmt, int ncol);
static void print_row_column(CliState *state, sqlite3_stmt *stmt, int ncol);
static void print_row_line(CliState *state, sqlite3_stmt *stmt, int ncol);
static void print_row_csv(CliState *state, sqlite3_stmt *stmt, int ncol);
static void print_headers(CliState *state, sqlite3_stmt *stmt, int ncol);
static char *trim(char *str);
static int str_starts_with(const char *str, const char *prefix);

/* Static strings for null display */
static char g_null_display[256] = "";

/* Initialize CLI state */
static void init_state(CliState *state) {
    state->db = NULL;
    state->db_filename = NULL;
    state->mode = MODE_LIST;
    strcpy(state->separator, "|");
    state->show_headers = false;
    state->echo_on = false;
    state->stats_on = false;
    state->bail_on_error = false;
    state->changes = 0;
    state->null_display = g_null_display;
    strcpy(state->prompt, "sqlite> ");
    strcpy(state->continue_prompt, "   ...> ");
    memset(state->col_widths, 0, sizeof(state->col_widths));
}

/* Clean up CLI state */
static void cleanup_state(CliState *state) {
    if (state->db) {
        sqlite3_close(state->db);
        state->db = NULL;
    }
    if (state->db_filename) {
        free(state->db_filename);
        state->db_filename = NULL;
    }
    /* null_display uses static buffer, no need to free */
}

/* Print usage information */
static void print_usage(const char *prog) {
    fprintf(stderr, "Usage: %s [OPTIONS] [DATABASE] [SQL]\n", prog);
    fprintf(stderr, "Options:\n");
    fprintf(stderr, "  -help              Show this help message\n");
    fprintf(stderr, "  -version           Show SQLite version\n");
    fprintf(stderr, "  -header            Turn headers on\n");
    fprintf(stderr, "  -noheader          Turn headers off\n");
    fprintf(stderr, "  -column            Set output mode to column\n");
    fprintf(stderr, "  -csv               Set output mode to csv\n");
    fprintf(stderr, "  -line              Set output mode to line\n");
    fprintf(stderr, "  -list              Set output mode to list\n");
    fprintf(stderr, "  -separator SEP     Set separator for list mode\n");
    fprintf(stderr, "  -nullvalue TEXT    Set text for NULL values\n");
    fprintf(stderr, "  -cmd COMMAND       Run SQL command before interactive mode\n");
    fprintf(stderr, "  -bail              Stop on error\n");
    fprintf(stderr, "  -echo              Print commands before execution\n");
}

/* Print version info */
static void print_version(void) {
    printf("SQLite version %s\n", sqlite3_libversion());
    printf("WASM Component CLI\n");
}

/* Print help */
static void print_help(void) {
    printf(".bail on|off           Stop after hitting an error\n");
    printf(".databases             List databases\n");
    printf(".dump ?TABLE?          Dump database in SQL format\n");
    printf(".echo on|off           Turn command echo on or off\n");
    printf(".exit                  Exit this program\n");
    printf(".headers on|off        Turn display of headers on or off\n");
    printf(".help                  Show this message\n");
    printf(".indexes ?TABLE?       Show indexes\n");
    printf(".mode MODE             Set output mode (list, column, csv, line, table, markdown)\n");
    printf(".nullvalue STRING      Use STRING in place of NULL values\n");
    printf(".open ?FILE?           Open database file\n");
    printf(".output ?FILE?         Send output to FILE (stdout if omitted)\n");
    printf(".print STRING...       Print literal STRING\n");
    printf(".prompt MAIN CONTINUE  Replace the standard prompts\n");
    printf(".quit                  Exit this program\n");
    printf(".read FILE             Execute SQL from FILE\n");
    printf(".schema ?TABLE?        Show CREATE statements\n");
    printf(".separator STRING      Set separator for list mode\n");
    printf(".show                  Show current settings\n");
    printf(".stats on|off          Turn stats on or off\n");
    printf(".tables ?PATTERN?      List tables matching PATTERN\n");
    printf(".version               Show SQLite version\n");
    printf(".width NUM NUM ...     Set column widths for column mode\n");
}

/* Trim whitespace from string */
static char *trim(char *str) {
    char *end;
    while (isspace((unsigned char)*str)) str++;
    if (*str == 0) return str;
    end = str + strlen(str) - 1;
    while (end > str && isspace((unsigned char)*end)) end--;
    *(end + 1) = '\0';
    return str;
}

/* Check if string starts with prefix (case-insensitive) */
static int str_starts_with(const char *str, const char *prefix) {
    while (*prefix) {
        if (tolower((unsigned char)*str) != tolower((unsigned char)*prefix)) {
            return 0;
        }
        str++;
        prefix++;
    }
    return 1;
}

/* Parse on/off argument */
static int parse_on_off(const char *arg) {
    if (strcasecmp(arg, "on") == 0 || strcasecmp(arg, "1") == 0 ||
        strcasecmp(arg, "yes") == 0 || strcasecmp(arg, "true") == 0) {
        return 1;
    }
    return 0;
}

/* Execute a dot command */
static int do_meta_command(CliState *state, const char *line) {
    char *cmd = strdup(line);
    char *arg1 = NULL;
    char *arg2 = NULL;
    char *p;
    int rc = 0;

    /* Skip the dot */
    p = cmd + 1;

    /* Parse command and arguments */
    char *saveptr;
    char *token = strtok_r(p, " \t\n", &saveptr);
    if (!token) {
        free(cmd);
        return 0;
    }

    char *cmd_name = token;
    arg1 = strtok_r(NULL, " \t\n", &saveptr);
    arg2 = strtok_r(NULL, " \t\n", &saveptr);

    if (strcasecmp(cmd_name, "help") == 0) {
        print_help();
    }
    else if (strcasecmp(cmd_name, "quit") == 0 || strcasecmp(cmd_name, "exit") == 0) {
        free(cmd);
        return -1;  /* Signal to exit */
    }
    else if (strcasecmp(cmd_name, "version") == 0) {
        print_version();
    }
    else if (strcasecmp(cmd_name, "headers") == 0) {
        if (arg1) {
            state->show_headers = parse_on_off(arg1);
        } else {
            fprintf(stderr, "Usage: .headers on|off\n");
        }
    }
    else if (strcasecmp(cmd_name, "mode") == 0) {
        if (!arg1) {
            printf("current mode: ");
            switch (state->mode) {
                case MODE_LIST: printf("list\n"); break;
                case MODE_COLUMN: printf("column\n"); break;
                case MODE_LINE: printf("line\n"); break;
                case MODE_CSV: printf("csv\n"); break;
                case MODE_TABLE: printf("table\n"); break;
                case MODE_MARKDOWN: printf("markdown\n"); break;
                case MODE_JSON: printf("json\n"); break;
            }
        } else if (strcasecmp(arg1, "list") == 0) {
            state->mode = MODE_LIST;
        } else if (strcasecmp(arg1, "column") == 0) {
            state->mode = MODE_COLUMN;
        } else if (strcasecmp(arg1, "line") == 0) {
            state->mode = MODE_LINE;
        } else if (strcasecmp(arg1, "csv") == 0) {
            state->mode = MODE_CSV;
        } else if (strcasecmp(arg1, "table") == 0) {
            state->mode = MODE_TABLE;
        } else if (strcasecmp(arg1, "markdown") == 0) {
            state->mode = MODE_MARKDOWN;
        } else if (strcasecmp(arg1, "json") == 0) {
            state->mode = MODE_JSON;
        } else {
            fprintf(stderr, "Unknown mode: %s\n", arg1);
            fprintf(stderr, "Valid modes: list, column, line, csv, table, markdown, json\n");
        }
    }
    else if (strcasecmp(cmd_name, "separator") == 0) {
        if (arg1) {
            strncpy(state->separator, arg1, sizeof(state->separator) - 1);
            state->separator[sizeof(state->separator) - 1] = '\0';
        } else {
            printf("current separator: \"%s\"\n", state->separator);
        }
    }
    else if (strcasecmp(cmd_name, "nullvalue") == 0) {
        if (arg1) {
            strncpy(g_null_display, arg1, sizeof(g_null_display) - 1);
            g_null_display[sizeof(g_null_display) - 1] = '\0';
        } else {
            printf("current nullvalue: \"%s\"\n", state->null_display);
        }
    }
    else if (strcasecmp(cmd_name, "echo") == 0) {
        if (arg1) {
            state->echo_on = parse_on_off(arg1);
        } else {
            printf("echo is %s\n", state->echo_on ? "on" : "off");
        }
    }
    else if (strcasecmp(cmd_name, "bail") == 0) {
        if (arg1) {
            state->bail_on_error = parse_on_off(arg1);
        } else {
            printf("bail is %s\n", state->bail_on_error ? "on" : "off");
        }
    }
    else if (strcasecmp(cmd_name, "stats") == 0) {
        if (arg1) {
            state->stats_on = parse_on_off(arg1);
        } else {
            printf("stats is %s\n", state->stats_on ? "on" : "off");
        }
    }
    else if (strcasecmp(cmd_name, "show") == 0) {
        printf("     echo: %s\n", state->echo_on ? "on" : "off");
        printf("  headers: %s\n", state->show_headers ? "on" : "off");
        printf("     mode: ");
        switch (state->mode) {
            case MODE_LIST: printf("list\n"); break;
            case MODE_COLUMN: printf("column\n"); break;
            case MODE_LINE: printf("line\n"); break;
            case MODE_CSV: printf("csv\n"); break;
            case MODE_TABLE: printf("table\n"); break;
            case MODE_MARKDOWN: printf("markdown\n"); break;
            case MODE_JSON: printf("json\n"); break;
        }
        printf("nullvalue: \"%s\"\n", state->null_display);
        printf("separator: \"%s\"\n", state->separator);
        printf("    stats: %s\n", state->stats_on ? "on" : "off");
    }
    else if (strcasecmp(cmd_name, "tables") == 0) {
        const char *sql;
        if (arg1) {
            char buf[1024];
            snprintf(buf, sizeof(buf),
                "SELECT name FROM sqlite_master "
                "WHERE type='table' AND name LIKE '%%%s%%' "
                "ORDER BY name", arg1);
            rc = execute_sql(state, buf);
        } else {
            sql = "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name";
            rc = execute_sql(state, sql);
        }
    }
    else if (strcasecmp(cmd_name, "schema") == 0) {
        char buf[1024];
        if (arg1) {
            snprintf(buf, sizeof(buf),
                "SELECT sql FROM sqlite_master WHERE name='%s'", arg1);
        } else {
            snprintf(buf, sizeof(buf),
                "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name");
        }
        rc = execute_sql(state, buf);
    }
    else if (strcasecmp(cmd_name, "indexes") == 0) {
        char buf[1024];
        if (arg1) {
            snprintf(buf, sizeof(buf),
                "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='%s' ORDER BY name",
                arg1);
        } else {
            snprintf(buf, sizeof(buf),
                "SELECT name FROM sqlite_master WHERE type='index' ORDER BY name");
        }
        rc = execute_sql(state, buf);
    }
    else if (strcasecmp(cmd_name, "databases") == 0) {
        rc = execute_sql(state, "PRAGMA database_list");
    }
    else if (strcasecmp(cmd_name, "open") == 0) {
        if (arg1) {
            if (state->db) {
                sqlite3_close(state->db);
                state->db = NULL;
            }
            if (state->db_filename) {
                free(state->db_filename);
            }
            state->db_filename = strdup(arg1);
            int rc2 = sqlite3_open_v2(arg1, &state->db,
                SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, NULL);
            if (rc2 != SQLITE_OK) {
                fprintf(stderr, "Error: cannot open \"%s\": %s\n",
                    arg1, sqlite3_errmsg(state->db));
                sqlite3_close(state->db);
                state->db = NULL;
            }
        } else {
            if (state->db_filename) {
                printf("current database: %s\n", state->db_filename);
            } else {
                printf("no database open\n");
            }
        }
    }
    else if (strcasecmp(cmd_name, "print") == 0) {
        if (arg1) {
            printf("%s", arg1);
            while ((arg1 = strtok_r(NULL, " \t\n", &saveptr)) != NULL) {
                printf(" %s", arg1);
            }
            printf("\n");
        }
    }
    else if (strcasecmp(cmd_name, "prompt") == 0) {
        if (arg1) {
            strncpy(state->prompt, arg1, sizeof(state->prompt) - 1);
            state->prompt[sizeof(state->prompt) - 1] = '\0';
        }
        if (arg2) {
            strncpy(state->continue_prompt, arg2, sizeof(state->continue_prompt) - 1);
            state->continue_prompt[sizeof(state->continue_prompt) - 1] = '\0';
        }
    }
    else if (strcasecmp(cmd_name, "width") == 0) {
        int i = 0;
        char *w = arg1;
        while (w && i < MAX_COLUMNS) {
            state->col_widths[i++] = atoi(w);
            w = strtok_r(NULL, " \t\n", &saveptr);
        }
    }
    else if (strcasecmp(cmd_name, "dump") == 0) {
        printf("BEGIN TRANSACTION;\n");
        char buf[1024];
        if (arg1) {
            snprintf(buf, sizeof(buf),
                "SELECT sql || ';' FROM sqlite_master WHERE name='%s' AND sql IS NOT NULL",
                arg1);
        } else {
            snprintf(buf, sizeof(buf),
                "SELECT sql || ';' FROM sqlite_master WHERE sql IS NOT NULL ORDER BY "
                "CASE type WHEN 'table' THEN 1 WHEN 'index' THEN 2 ELSE 3 END, name");
        }
        OutputMode old_mode = state->mode;
        bool old_headers = state->show_headers;
        state->mode = MODE_LIST;
        state->show_headers = false;
        execute_sql(state, buf);
        state->mode = old_mode;
        state->show_headers = old_headers;
        printf("COMMIT;\n");
    }
    else {
        fprintf(stderr, "Error: unknown command: .%s\n", cmd_name);
        fprintf(stderr, "Use .help for a list of commands\n");
    }

    free(cmd);
    return rc;
}

/* Print column headers */
static void print_headers(CliState *state, sqlite3_stmt *stmt, int ncol) {
    switch (state->mode) {
        case MODE_LIST:
            for (int i = 0; i < ncol; i++) {
                if (i > 0) printf("%s", state->separator);
                printf("%s", sqlite3_column_name(stmt, i));
            }
            printf("\n");
            break;

        case MODE_CSV:
            for (int i = 0; i < ncol; i++) {
                if (i > 0) printf(",");
                const char *name = sqlite3_column_name(stmt, i);
                /* Quote if contains comma, quote, or newline */
                if (strchr(name, ',') || strchr(name, '"') || strchr(name, '\n')) {
                    printf("\"");
                    for (const char *p = name; *p; p++) {
                        if (*p == '"') printf("\"\"");
                        else putchar(*p);
                    }
                    printf("\"");
                } else {
                    printf("%s", name);
                }
            }
            printf("\n");
            break;

        case MODE_COLUMN:
        case MODE_TABLE:
        case MODE_MARKDOWN:
            for (int i = 0; i < ncol; i++) {
                int width = state->col_widths[i];
                if (width <= 0) width = 10;
                if (state->mode == MODE_TABLE || state->mode == MODE_MARKDOWN) {
                    printf("| ");
                }
                printf("%-*s", width, sqlite3_column_name(stmt, i));
                if (state->mode != MODE_TABLE && state->mode != MODE_MARKDOWN) {
                    printf("  ");
                } else {
                    printf(" ");
                }
            }
            if (state->mode == MODE_TABLE || state->mode == MODE_MARKDOWN) {
                printf("|");
            }
            printf("\n");
            /* Print separator line */
            for (int i = 0; i < ncol; i++) {
                int width = state->col_widths[i];
                if (width <= 0) width = 10;
                if (state->mode == MODE_TABLE || state->mode == MODE_MARKDOWN) {
                    printf("|");
                    for (int j = 0; j < width + 2; j++) printf("-");
                } else {
                    for (int j = 0; j < width; j++) printf("-");
                    printf("  ");
                }
            }
            if (state->mode == MODE_TABLE || state->mode == MODE_MARKDOWN) {
                printf("|");
            }
            printf("\n");
            break;

        case MODE_LINE:
            /* No headers in line mode */
            break;

        case MODE_JSON:
            /* JSON headers handled differently */
            break;
    }
}

/* Print a row in list mode */
static void print_row_list(CliState *state, sqlite3_stmt *stmt, int ncol) {
    for (int i = 0; i < ncol; i++) {
        if (i > 0) printf("%s", state->separator);
        if (sqlite3_column_type(stmt, i) == SQLITE_NULL) {
            printf("%s", state->null_display);
        } else {
            printf("%s", (const char *)sqlite3_column_text(stmt, i));
        }
    }
    printf("\n");
}

/* Print a row in column mode */
static void print_row_column(CliState *state, sqlite3_stmt *stmt, int ncol) {
    for (int i = 0; i < ncol; i++) {
        int width = state->col_widths[i];
        if (width <= 0) width = 10;
        if (state->mode == MODE_TABLE || state->mode == MODE_MARKDOWN) {
            printf("| ");
        }
        const char *val;
        if (sqlite3_column_type(stmt, i) == SQLITE_NULL) {
            val = state->null_display;
        } else {
            val = (const char *)sqlite3_column_text(stmt, i);
        }
        printf("%-*.*s", width, width, val ? val : "");
        if (state->mode != MODE_TABLE && state->mode != MODE_MARKDOWN) {
            printf("  ");
        } else {
            printf(" ");
        }
    }
    if (state->mode == MODE_TABLE || state->mode == MODE_MARKDOWN) {
        printf("|");
    }
    printf("\n");
}

/* Print a row in line mode */
static void print_row_line(CliState *state, sqlite3_stmt *stmt, int ncol) {
    for (int i = 0; i < ncol; i++) {
        const char *name = sqlite3_column_name(stmt, i);
        const char *val;
        if (sqlite3_column_type(stmt, i) == SQLITE_NULL) {
            val = state->null_display;
        } else {
            val = (const char *)sqlite3_column_text(stmt, i);
        }
        printf("%12s = %s\n", name, val ? val : "");
    }
    printf("\n");
}

/* Print a row in CSV mode */
static void print_row_csv(CliState *state, sqlite3_stmt *stmt, int ncol) {
    for (int i = 0; i < ncol; i++) {
        if (i > 0) printf(",");
        if (sqlite3_column_type(stmt, i) == SQLITE_NULL) {
            /* Empty for NULL in CSV */
        } else {
            const char *val = (const char *)sqlite3_column_text(stmt, i);
            if (val && (strchr(val, ',') || strchr(val, '"') || strchr(val, '\n'))) {
                printf("\"");
                for (const char *p = val; *p; p++) {
                    if (*p == '"') printf("\"\"");
                    else putchar(*p);
                }
                printf("\"");
            } else {
                printf("%s", val ? val : "");
            }
        }
    }
    printf("\n");
}

/* Execute SQL and display results */
static int execute_sql(CliState *state, const char *sql) {
    sqlite3_stmt *stmt = NULL;
    const char *tail = sql;
    int rc = SQLITE_OK;
    int first_json = 1;

    if (!state->db) {
        fprintf(stderr, "Error: no database open\n");
        return 1;
    }

    while (tail && *tail) {
        /* Skip whitespace */
        while (*tail && isspace((unsigned char)*tail)) tail++;
        if (!*tail) break;

        rc = sqlite3_prepare_v2(state->db, tail, -1, &stmt, &tail);
        if (rc != SQLITE_OK) {
            fprintf(stderr, "Error: %s\n", sqlite3_errmsg(state->db));
            return 1;
        }

        if (!stmt) continue;  /* Empty statement */

        int ncol = sqlite3_column_count(stmt);
        int row_count = 0;
        bool headers_printed = false;

        if (state->mode == MODE_JSON && ncol > 0) {
            printf("[");
        }

        while ((rc = sqlite3_step(stmt)) == SQLITE_ROW) {
            if (ncol > 0) {
                /* Print headers on first row */
                if (!headers_printed && state->show_headers) {
                    print_headers(state, stmt, ncol);
                    headers_printed = true;
                }

                switch (state->mode) {
                    case MODE_LIST:
                        print_row_list(state, stmt, ncol);
                        break;
                    case MODE_COLUMN:
                    case MODE_TABLE:
                    case MODE_MARKDOWN:
                        print_row_column(state, stmt, ncol);
                        break;
                    case MODE_LINE:
                        print_row_line(state, stmt, ncol);
                        break;
                    case MODE_CSV:
                        print_row_csv(state, stmt, ncol);
                        break;
                    case MODE_JSON:
                        if (row_count > 0) printf(",");
                        printf("{");
                        for (int i = 0; i < ncol; i++) {
                            if (i > 0) printf(",");
                            printf("\"%s\":", sqlite3_column_name(stmt, i));
                            int type = sqlite3_column_type(stmt, i);
                            if (type == SQLITE_NULL) {
                                printf("null");
                            } else if (type == SQLITE_INTEGER) {
                                printf("%lld", sqlite3_column_int64(stmt, i));
                            } else if (type == SQLITE_FLOAT) {
                                printf("%g", sqlite3_column_double(stmt, i));
                            } else {
                                const char *val = (const char *)sqlite3_column_text(stmt, i);
                                printf("\"");
                                for (const char *p = val; p && *p; p++) {
                                    switch (*p) {
                                        case '"': printf("\\\""); break;
                                        case '\\': printf("\\\\"); break;
                                        case '\n': printf("\\n"); break;
                                        case '\r': printf("\\r"); break;
                                        case '\t': printf("\\t"); break;
                                        default: putchar(*p);
                                    }
                                }
                                printf("\"");
                            }
                        }
                        printf("}");
                        break;
                }
            }
            row_count++;
        }

        if (state->mode == MODE_JSON && ncol > 0) {
            printf("]\n");
        }

        if (rc != SQLITE_DONE && rc != SQLITE_ROW) {
            fprintf(stderr, "Error: %s\n", sqlite3_errmsg(state->db));
            sqlite3_finalize(stmt);
            return 1;
        }

        state->changes = sqlite3_changes(state->db);

        if (state->stats_on) {
            printf("Rows returned: %d\n", row_count);
            printf("Changes: %d\n", state->changes);
        }

        sqlite3_finalize(stmt);
        stmt = NULL;
    }

    return 0;
}

/* Check if SQL is complete (has all necessary semicolons/terminators) */
static int sql_is_complete(const char *sql) {
    return sqlite3_complete(sql);
}

/* Read a line from stdin */
static char *read_line(const char *prompt) {
    static char line[MAX_LINE];

    if (prompt) {
        printf("%s", prompt);
        fflush(stdout);
    }

    if (fgets(line, sizeof(line), stdin) == NULL) {
        return NULL;
    }

    return line;
}

/* Static SQL buffer for REPL */
static char g_sql_buffer[MAX_SQL_BUFFER];

/* Main REPL loop */
static int repl(CliState *state) {
    g_sql_buffer[0] = '\0';
    bool in_sql = false;

    while (1) {
        const char *prompt = in_sql ? state->continue_prompt : state->prompt;
        char *line = read_line(prompt);

        if (line == NULL) {
            /* EOF */
            if (in_sql) {
                fprintf(stderr, "Error: incomplete SQL\n");
            }
            break;
        }

        char *trimmed = trim(line);
        if (*trimmed == '\0') {
            continue;
        }

        if (state->echo_on) {
            printf("%s\n", trimmed);
        }

        /* Check for dot command (only at start of input) */
        if (!in_sql && trimmed[0] == '.') {
            int rc = do_meta_command(state, trimmed);
            if (rc < 0) {
                /* Exit requested */
                break;
            }
            continue;
        }

        /* Accumulate SQL */
        if (strlen(g_sql_buffer) + strlen(trimmed) + 2 < sizeof(g_sql_buffer)) {
            if (g_sql_buffer[0] != '\0') {
                strcat(g_sql_buffer, " ");
            }
            strcat(g_sql_buffer, trimmed);
        } else {
            fprintf(stderr, "Error: SQL too long\n");
            g_sql_buffer[0] = '\0';
            in_sql = false;
            continue;
        }

        /* Check if SQL is complete */
        if (sql_is_complete(g_sql_buffer)) {
            int rc = execute_sql(state, g_sql_buffer);
            g_sql_buffer[0] = '\0';
            in_sql = false;
            if (rc != 0 && state->bail_on_error) {
                return 1;
            }
        } else {
            in_sql = true;
        }
    }

    return 0;
}

/* Main entry point */
int main(int argc, char **argv) {
    CliState state;
    init_state(&state);

    const char *init_sql = NULL;
    const char *cmd_sql = NULL;
    bool interactive = true;
    int i;

    /* Parse command line arguments */
    for (i = 1; i < argc; i++) {
        if (argv[i][0] == '-') {
            if (strcmp(argv[i], "-help") == 0 || strcmp(argv[i], "--help") == 0) {
                print_usage(argv[0]);
                cleanup_state(&state);
                return 0;
            }
            else if (strcmp(argv[i], "-version") == 0 || strcmp(argv[i], "--version") == 0) {
                print_version();
                cleanup_state(&state);
                return 0;
            }
            else if (strcmp(argv[i], "-header") == 0) {
                state.show_headers = true;
            }
            else if (strcmp(argv[i], "-noheader") == 0) {
                state.show_headers = false;
            }
            else if (strcmp(argv[i], "-column") == 0) {
                state.mode = MODE_COLUMN;
            }
            else if (strcmp(argv[i], "-csv") == 0) {
                state.mode = MODE_CSV;
            }
            else if (strcmp(argv[i], "-line") == 0) {
                state.mode = MODE_LINE;
            }
            else if (strcmp(argv[i], "-list") == 0) {
                state.mode = MODE_LIST;
            }
            else if (strcmp(argv[i], "-separator") == 0 && i + 1 < argc) {
                strncpy(state.separator, argv[++i], sizeof(state.separator) - 1);
            }
            else if (strcmp(argv[i], "-nullvalue") == 0 && i + 1 < argc) {
                strncpy(g_null_display, argv[++i], sizeof(g_null_display) - 1);
                g_null_display[sizeof(g_null_display) - 1] = '\0';
            }
            else if (strcmp(argv[i], "-cmd") == 0 && i + 1 < argc) {
                cmd_sql = argv[++i];
            }
            else if (strcmp(argv[i], "-bail") == 0) {
                state.bail_on_error = true;
            }
            else if (strcmp(argv[i], "-echo") == 0) {
                state.echo_on = true;
            }
            else {
                fprintf(stderr, "Unknown option: %s\n", argv[i]);
                print_usage(argv[0]);
                cleanup_state(&state);
                return 1;
            }
        } else {
            /* First non-option is database name */
            if (state.db_filename == NULL) {
                state.db_filename = strdup(argv[i]);
            } else {
                /* Second non-option is SQL to execute */
                init_sql = argv[i];
                interactive = false;
            }
        }
    }

    /* Open database */
    if (state.db_filename) {
        int rc = sqlite3_open_v2(state.db_filename, &state.db,
            SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, NULL);
        if (rc != SQLITE_OK) {
            fprintf(stderr, "Error: cannot open \"%s\": %s\n",
                state.db_filename, state.db ? sqlite3_errmsg(state.db) : "unknown error");
            cleanup_state(&state);
            return 1;
        }
    } else {
        /* Default to in-memory database */
        state.db_filename = strdup(":memory:");
        int rc = sqlite3_open(":memory:", &state.db);
        if (rc != SQLITE_OK) {
            fprintf(stderr, "Error: cannot open in-memory database: %s\n",
                state.db ? sqlite3_errmsg(state.db) : "unknown error");
            cleanup_state(&state);
            return 1;
        }
    }

    /* Execute -cmd SQL if provided */
    if (cmd_sql) {
        if (cmd_sql[0] == '.') {
            do_meta_command(&state, cmd_sql);
        } else {
            execute_sql(&state, cmd_sql);
        }
    }

    /* Execute SQL from command line or run REPL */
    int result = 0;
    if (init_sql) {
        result = execute_sql(&state, init_sql);
    } else if (interactive) {
        printf("SQLite version %s\n", sqlite3_libversion());
        printf("Enter \".help\" for usage hints.\n");
        if (strcmp(state.db_filename, ":memory:") == 0) {
            printf("Connected to a transient in-memory database.\n");
            printf("Use \".open FILENAME\" to reopen on a persistent database.\n");
        }
        result = repl(&state);
    }

    cleanup_state(&state);
    return result;
}
