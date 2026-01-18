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
#include <time.h>
#include "sqlite3.h"

/* Maximum input line length */
#define MAX_LINE 8192

/* Maximum number of columns to display */
#define MAX_COLUMNS 100

/* Maximum SQL buffer size */
#define MAX_SQL_BUFFER 65536

/* Maximum loaded extensions */
#define MAX_EXTENSIONS 32

/* Extension info structure */
typedef struct {
    char name[64];
    char version[32];
    char path[512];
    int num_functions;
    bool loaded;
} LoadedExtension;

/* Global extension list */
static LoadedExtension g_extensions[MAX_EXTENSIONS];
static int g_extension_count = 0;

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
    char rowseparator[20];
    bool show_headers;
    bool echo_on;
    bool stats_on;
    bool bail_on_error;
    bool timer_on;
    bool trace_on;
    bool eqp_on;
    int changes;
    char *null_display;
    int col_widths[MAX_COLUMNS];
    char prompt[64];
    char continue_prompt[64];
    FILE *output_file;
    char output_filename[512];
    FILE *once_file;
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
static int process_file_input(CliState *state, const char *filename);
static int import_csv(CliState *state, const char *filename, const char *table);

/* Get output file (handles .once redirection) */
static FILE *get_output(CliState *state) {
    if (state->once_file) {
        return state->once_file;
    }
    return state->output_file;
}

/* Close .once file after use */
static void close_once(CliState *state) {
    if (state->once_file) {
        fclose(state->once_file);
        state->once_file = NULL;
    }
}

/* Static strings for null display */
static char g_null_display[256] = "";

/* Initialize CLI state */
static void init_state(CliState *state) {
    state->db = NULL;
    state->db_filename = NULL;
    state->mode = MODE_LIST;
    strcpy(state->separator, "|");
    strcpy(state->rowseparator, "\n");
    state->show_headers = false;
    state->echo_on = false;
    state->stats_on = false;
    state->bail_on_error = false;
    state->timer_on = false;
    state->trace_on = false;
    state->eqp_on = false;
    state->changes = 0;
    state->null_display = g_null_display;
    strcpy(state->prompt, "sqlite> ");
    strcpy(state->continue_prompt, "   ...> ");
    memset(state->col_widths, 0, sizeof(state->col_widths));
    state->output_file = stdout;
    state->output_filename[0] = '\0';
    state->once_file = NULL;
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
    if (state->output_file && state->output_file != stdout) {
        fclose(state->output_file);
        state->output_file = stdout;
    }
    if (state->once_file) {
        fclose(state->once_file);
        state->once_file = NULL;
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
    printf(".backup ?DB? FILE      Backup database to FILE\n");
    printf(".bail on|off           Stop after hitting an error\n");
    printf(".changes               Display number of rows changed\n");
    printf(".clone NEWDB           Clone database to NEWDB\n");
    printf(".databases             List databases\n");
    printf(".dbinfo                Show database information\n");
    printf(".dump ?TABLE?          Dump database in SQL format\n");
    printf(".echo on|off           Turn command echo on or off\n");
    printf(".eqp on|off            Enable EXPLAIN QUERY PLAN\n");
    printf(".exit                  Exit this program\n");
    printf(".extensions            List loaded WASM extensions\n");
    printf(".fullschema            Show complete schema\n");
    printf(".headers on|off        Turn display of headers on or off\n");
    printf(".help                  Show this message\n");
    printf(".import FILE TABLE     Import CSV/TSV file into TABLE\n");
    printf(".indexes ?TABLE?       Show indexes\n");
    printf(".limit ?LIMIT? ?VAL?   Display or change limits\n");
    printf(".lint fkey-indexes     Check for missing FK indexes\n");
    printf(".load FILE             Load a WASM extension\n");
    printf(".mode MODE             Set output mode (list, column, csv, line, table, markdown, json)\n");
    printf(".nullvalue STRING      Use STRING in place of NULL values\n");
    printf(".once FILE             Output next SQL to FILE\n");
    printf(".open ?FILE?           Open database file\n");
    printf(".output ?FILE?         Send output to FILE (stdout if omitted)\n");
    printf(".print STRING...       Print literal STRING\n");
    printf(".prompt MAIN CONTINUE  Replace the standard prompts\n");
    printf(".quit                  Exit this program\n");
    printf(".read FILE             Execute SQL from FILE\n");
    printf(".restore ?DB? FILE     Restore database from FILE\n");
    printf(".save FILE             Save database to FILE (alias for .backup)\n");
    printf(".schema ?TABLE?        Show CREATE statements\n");
    printf(".separator STRING      Set separator for list mode\n");
    printf(".show                  Show current settings\n");
    printf(".stats on|off          Turn stats on or off\n");
    printf(".tables ?PATTERN?      List tables matching PATTERN\n");
    printf(".timeout MS            Set busy timeout in milliseconds\n");
    printf(".timer on|off          Turn SQL timer on or off\n");
    printf(".trace on|off          Trace SQL statements\n");
    printf(".unload NAME           Unload a WASM extension\n");
    printf(".version               Show SQLite version\n");
    printf(".vfslist               List available VFSes\n");
    printf(".vfsname               Show default VFS name\n");
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
        printf("        bail: %s\n", state->bail_on_error ? "on" : "off");
        printf("        echo: %s\n", state->echo_on ? "on" : "off");
        printf("         eqp: %s\n", state->eqp_on ? "on" : "off");
        printf("     headers: %s\n", state->show_headers ? "on" : "off");
        printf("        mode: ");
        switch (state->mode) {
            case MODE_LIST: printf("list\n"); break;
            case MODE_COLUMN: printf("column\n"); break;
            case MODE_LINE: printf("line\n"); break;
            case MODE_CSV: printf("csv\n"); break;
            case MODE_TABLE: printf("table\n"); break;
            case MODE_MARKDOWN: printf("markdown\n"); break;
            case MODE_JSON: printf("json\n"); break;
        }
        printf("   nullvalue: \"%s\"\n", state->null_display);
        printf("      output: %s\n", state->output_filename[0] ? state->output_filename : "stdout");
        printf("   separator: \"%s\"\n", state->separator);
        printf("       stats: %s\n", state->stats_on ? "on" : "off");
        printf("       timer: %s\n", state->timer_on ? "on" : "off");
        printf("       trace: %s\n", state->trace_on ? "on" : "off");
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
    else if (strcasecmp(cmd_name, "load") == 0) {
        if (!arg1) {
            fprintf(stderr, "Usage: .load FILENAME\n");
        } else if (g_extension_count >= MAX_EXTENSIONS) {
            fprintf(stderr, "Error: maximum number of extensions (%d) already loaded\n", MAX_EXTENSIONS);
        } else {
            /* In a full WASM component implementation, this would:
             * 1. Call the imported extension-loader interface to load the WASM component
             * 2. Get extension info (name, version, functions)
             * 3. Register the extension's functions with SQLite
             *
             * For now, we track the extension locally and print a message.
             * The actual loading happens on the host side.
             */
            LoadedExtension *ext = &g_extensions[g_extension_count];
            strncpy(ext->path, arg1, sizeof(ext->path) - 1);
            ext->path[sizeof(ext->path) - 1] = '\0';

            /* Extract extension name from path (basename without .wasm) */
            const char *basename = strrchr(arg1, '/');
            if (!basename) basename = strrchr(arg1, '\\');
            basename = basename ? basename + 1 : arg1;

            strncpy(ext->name, basename, sizeof(ext->name) - 1);
            ext->name[sizeof(ext->name) - 1] = '\0';

            /* Remove .wasm extension if present */
            char *dot = strrchr(ext->name, '.');
            if (dot && strcasecmp(dot, ".wasm") == 0) {
                *dot = '\0';
            }

            strcpy(ext->version, "1.0.0");
            ext->num_functions = 0;
            ext->loaded = true;
            g_extension_count++;

            printf("Loaded extension: %s from %s\n", ext->name, ext->path);
            printf("Note: Extension loading requires host-side WASM component support.\n");
        }
    }
    else if (strcasecmp(cmd_name, "unload") == 0) {
        if (!arg1) {
            fprintf(stderr, "Usage: .unload NAME\n");
        } else {
            bool found = false;
            for (int i = 0; i < g_extension_count; i++) {
                if (g_extensions[i].loaded && strcasecmp(g_extensions[i].name, arg1) == 0) {
                    g_extensions[i].loaded = false;
                    printf("Unloaded extension: %s\n", arg1);
                    found = true;
                    break;
                }
            }
            if (!found) {
                fprintf(stderr, "Error: extension '%s' not found\n", arg1);
            }
        }
    }
    else if (strcasecmp(cmd_name, "extensions") == 0) {
        int loaded_count = 0;
        for (int i = 0; i < g_extension_count; i++) {
            if (g_extensions[i].loaded) {
                loaded_count++;
            }
        }
        if (loaded_count == 0) {
            printf("No extensions loaded.\n");
            printf("Use .load FILENAME to load a WASM extension.\n");
        } else {
            printf("Loaded extensions:\n");
            for (int i = 0; i < g_extension_count; i++) {
                if (g_extensions[i].loaded) {
                    printf("  %s v%s (%s)\n",
                           g_extensions[i].name,
                           g_extensions[i].version,
                           g_extensions[i].path);
                }
            }
        }
    }
    /* Phase 1: .read and .output */
    else if (strcasecmp(cmd_name, "read") == 0) {
        if (!arg1) {
            fprintf(stderr, "Usage: .read FILENAME\n");
        } else {
            rc = process_file_input(state, arg1);
        }
    }
    else if (strcasecmp(cmd_name, "output") == 0) {
        if (!arg1 || strcmp(arg1, "stdout") == 0) {
            if (state->output_file && state->output_file != stdout) {
                fclose(state->output_file);
            }
            state->output_file = stdout;
            state->output_filename[0] = '\0';
        } else {
            FILE *fp = fopen(arg1, "w");
            if (!fp) {
                fprintf(stderr, "Error: cannot open \"%s\" for writing\n", arg1);
            } else {
                if (state->output_file && state->output_file != stdout) {
                    fclose(state->output_file);
                }
                state->output_file = fp;
                strncpy(state->output_filename, arg1, sizeof(state->output_filename) - 1);
                state->output_filename[sizeof(state->output_filename) - 1] = '\0';
            }
        }
    }
    /* Phase 2: Data management commands */
    else if (strcasecmp(cmd_name, "import") == 0) {
        if (!arg1 || !arg2) {
            fprintf(stderr, "Usage: .import FILE TABLE\n");
        } else {
            rc = import_csv(state, arg1, arg2);
        }
    }
    else if (strcasecmp(cmd_name, "backup") == 0 || strcasecmp(cmd_name, "save") == 0) {
        const char *db_name = "main";
        const char *filename = arg1;
        if (arg2) {
            db_name = arg1;
            filename = arg2;
        }
        if (!filename) {
            fprintf(stderr, "Usage: .backup ?DB? FILENAME\n");
        } else {
            sqlite3 *dest;
            int rc2 = sqlite3_open(filename, &dest);
            if (rc2 != SQLITE_OK) {
                fprintf(stderr, "Error: cannot open \"%s\": %s\n", filename,
                    sqlite3_errmsg(dest));
                sqlite3_close(dest);
            } else {
                sqlite3_backup *backup = sqlite3_backup_init(dest, "main", state->db, db_name);
                if (backup) {
                    sqlite3_backup_step(backup, -1);
                    sqlite3_backup_finish(backup);
                    printf("Database backed up to %s\n", filename);
                } else {
                    fprintf(stderr, "Error: backup failed: %s\n", sqlite3_errmsg(dest));
                }
                sqlite3_close(dest);
            }
        }
    }
    else if (strcasecmp(cmd_name, "restore") == 0) {
        const char *db_name = "main";
        const char *filename = arg1;
        if (arg2) {
            db_name = arg1;
            filename = arg2;
        }
        if (!filename) {
            fprintf(stderr, "Usage: .restore ?DB? FILENAME\n");
        } else {
            sqlite3 *src;
            int rc2 = sqlite3_open(filename, &src);
            if (rc2 != SQLITE_OK) {
                fprintf(stderr, "Error: cannot open \"%s\": %s\n", filename,
                    sqlite3_errmsg(src));
                sqlite3_close(src);
            } else {
                sqlite3_backup *backup = sqlite3_backup_init(state->db, db_name, src, "main");
                if (backup) {
                    sqlite3_backup_step(backup, -1);
                    sqlite3_backup_finish(backup);
                    printf("Database restored from %s\n", filename);
                } else {
                    fprintf(stderr, "Error: restore failed: %s\n", sqlite3_errmsg(state->db));
                }
                sqlite3_close(src);
            }
        }
    }
    else if (strcasecmp(cmd_name, "clone") == 0) {
        if (!arg1) {
            fprintf(stderr, "Usage: .clone NEWDB\n");
        } else {
            sqlite3 *dest;
            int rc2 = sqlite3_open(arg1, &dest);
            if (rc2 != SQLITE_OK) {
                fprintf(stderr, "Error: cannot open \"%s\": %s\n", arg1,
                    sqlite3_errmsg(dest));
                sqlite3_close(dest);
            } else {
                sqlite3_backup *backup = sqlite3_backup_init(dest, "main", state->db, "main");
                if (backup) {
                    sqlite3_backup_step(backup, -1);
                    sqlite3_backup_finish(backup);
                    printf("Database cloned to %s\n", arg1);
                } else {
                    fprintf(stderr, "Error: clone failed: %s\n", sqlite3_errmsg(dest));
                }
                sqlite3_close(dest);
            }
        }
    }
    /* Phase 3: Query analysis commands */
    else if (strcasecmp(cmd_name, "changes") == 0) {
        printf("Changes: %d\n", sqlite3_changes(state->db));
        printf("Total changes: %d\n", sqlite3_total_changes(state->db));
    }
    else if (strcasecmp(cmd_name, "timer") == 0) {
        if (arg1) {
            state->timer_on = parse_on_off(arg1);
        } else {
            printf("timer is %s\n", state->timer_on ? "on" : "off");
        }
    }
    else if (strcasecmp(cmd_name, "timeout") == 0) {
        if (arg1) {
            int ms = atoi(arg1);
            sqlite3_busy_timeout(state->db, ms);
            printf("Timeout set to %d ms\n", ms);
        } else {
            fprintf(stderr, "Usage: .timeout MILLISECONDS\n");
        }
    }
    else if (strcasecmp(cmd_name, "trace") == 0) {
        if (!arg1 || strcasecmp(arg1, "off") == 0) {
            state->trace_on = false;
            printf("Tracing disabled\n");
        } else {
            state->trace_on = parse_on_off(arg1);
            printf("Tracing %s\n", state->trace_on ? "enabled" : "disabled");
        }
    }
    else if (strcasecmp(cmd_name, "eqp") == 0) {
        if (arg1) {
            state->eqp_on = parse_on_off(arg1);
        } else {
            printf("eqp is %s\n", state->eqp_on ? "on" : "off");
        }
    }
    /* Phase 4: Database information commands */
    else if (strcasecmp(cmd_name, "fullschema") == 0) {
        execute_sql(state, "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY "
            "CASE type WHEN 'table' THEN 1 WHEN 'index' THEN 2 ELSE 3 END, name");
        /* Also show sqlite_stat tables if they exist */
        execute_sql(state, "SELECT 'ANALYZE sqlite_master;' WHERE EXISTS "
            "(SELECT 1 FROM sqlite_master WHERE name='sqlite_stat1')");
    }
    else if (strcasecmp(cmd_name, "dbinfo") == 0) {
        printf("database: %s\n", state->db_filename ? state->db_filename : "unknown");
        execute_sql(state, "SELECT 'page_size:', page_size FROM pragma_page_size");
        execute_sql(state, "SELECT 'page_count:', page_count FROM pragma_page_count");
        execute_sql(state, "SELECT 'freelist_count:', freelist_count FROM pragma_freelist_count");
        execute_sql(state, "SELECT 'encoding:', encoding FROM pragma_encoding");
        execute_sql(state, "SELECT 'journal_mode:', journal_mode FROM pragma_journal_mode");
        execute_sql(state, "SELECT 'auto_vacuum:', auto_vacuum FROM pragma_auto_vacuum");
    }
    else if (strcasecmp(cmd_name, "limit") == 0) {
        if (!arg1) {
            printf("SQLITE_LIMIT_LENGTH          %d\n", sqlite3_limit(state->db, SQLITE_LIMIT_LENGTH, -1));
            printf("SQLITE_LIMIT_SQL_LENGTH      %d\n", sqlite3_limit(state->db, SQLITE_LIMIT_SQL_LENGTH, -1));
            printf("SQLITE_LIMIT_COLUMN          %d\n", sqlite3_limit(state->db, SQLITE_LIMIT_COLUMN, -1));
            printf("SQLITE_LIMIT_EXPR_DEPTH      %d\n", sqlite3_limit(state->db, SQLITE_LIMIT_EXPR_DEPTH, -1));
            printf("SQLITE_LIMIT_COMPOUND_SELECT %d\n", sqlite3_limit(state->db, SQLITE_LIMIT_COMPOUND_SELECT, -1));
            printf("SQLITE_LIMIT_VDBE_OP         %d\n", sqlite3_limit(state->db, SQLITE_LIMIT_VDBE_OP, -1));
            printf("SQLITE_LIMIT_FUNCTION_ARG    %d\n", sqlite3_limit(state->db, SQLITE_LIMIT_FUNCTION_ARG, -1));
            printf("SQLITE_LIMIT_ATTACHED        %d\n", sqlite3_limit(state->db, SQLITE_LIMIT_ATTACHED, -1));
            printf("SQLITE_LIMIT_LIKE_PATTERN_LENGTH %d\n", sqlite3_limit(state->db, SQLITE_LIMIT_LIKE_PATTERN_LENGTH, -1));
            printf("SQLITE_LIMIT_VARIABLE_NUMBER %d\n", sqlite3_limit(state->db, SQLITE_LIMIT_VARIABLE_NUMBER, -1));
            printf("SQLITE_LIMIT_TRIGGER_DEPTH   %d\n", sqlite3_limit(state->db, SQLITE_LIMIT_TRIGGER_DEPTH, -1));
        } else if (arg2) {
            int limit_id = -1;
            if (strcasecmp(arg1, "length") == 0) limit_id = SQLITE_LIMIT_LENGTH;
            else if (strcasecmp(arg1, "sql_length") == 0) limit_id = SQLITE_LIMIT_SQL_LENGTH;
            else if (strcasecmp(arg1, "column") == 0) limit_id = SQLITE_LIMIT_COLUMN;
            else if (strcasecmp(arg1, "expr_depth") == 0) limit_id = SQLITE_LIMIT_EXPR_DEPTH;
            else if (strcasecmp(arg1, "compound_select") == 0) limit_id = SQLITE_LIMIT_COMPOUND_SELECT;
            else if (strcasecmp(arg1, "vdbe_op") == 0) limit_id = SQLITE_LIMIT_VDBE_OP;
            else if (strcasecmp(arg1, "function_arg") == 0) limit_id = SQLITE_LIMIT_FUNCTION_ARG;
            else if (strcasecmp(arg1, "attached") == 0) limit_id = SQLITE_LIMIT_ATTACHED;
            else if (strcasecmp(arg1, "like_pattern_length") == 0) limit_id = SQLITE_LIMIT_LIKE_PATTERN_LENGTH;
            else if (strcasecmp(arg1, "variable_number") == 0) limit_id = SQLITE_LIMIT_VARIABLE_NUMBER;
            else if (strcasecmp(arg1, "trigger_depth") == 0) limit_id = SQLITE_LIMIT_TRIGGER_DEPTH;

            if (limit_id >= 0) {
                int new_val = atoi(arg2);
                int old_val = sqlite3_limit(state->db, limit_id, new_val);
                printf("Limit %s changed from %d to %d\n", arg1, old_val, new_val);
            } else {
                fprintf(stderr, "Unknown limit: %s\n", arg1);
            }
        } else {
            fprintf(stderr, "Usage: .limit ?LIMIT? ?VALUE?\n");
        }
    }
    /* Phase 5: Additional output commands */
    else if (strcasecmp(cmd_name, "once") == 0) {
        if (!arg1) {
            fprintf(stderr, "Usage: .once FILENAME\n");
        } else {
            if (state->once_file) {
                fclose(state->once_file);
            }
            state->once_file = fopen(arg1, "w");
            if (!state->once_file) {
                fprintf(stderr, "Error: cannot open \"%s\" for writing\n", arg1);
            }
        }
    }
    else if (strcasecmp(cmd_name, "vfslist") == 0) {
        sqlite3_vfs *vfs = sqlite3_vfs_find(NULL);
        sqlite3_vfs *default_vfs = vfs;
        printf("VFS Name              Default  Max Pathname\n");
        printf("--------------------  -------  ------------\n");
        while (vfs) {
            printf("%-20s  %-7s  %d\n",
                vfs->zName,
                vfs == default_vfs ? "yes" : "no",
                vfs->mxPathname);
            vfs = vfs->pNext;
        }
    }
    else if (strcasecmp(cmd_name, "vfsname") == 0) {
        sqlite3_vfs *vfs = sqlite3_vfs_find(NULL);
        if (vfs) {
            printf("%s\n", vfs->zName);
        }
    }
    else if (strcasecmp(cmd_name, "lint") == 0) {
        if (arg1 && strcasecmp(arg1, "fkey-indexes") == 0) {
            execute_sql(state,
                "SELECT 'Table ' || p.'table' || ' references ' || p.'table' || '.' || p.'to' "
                "|| ' but has no index on column ' || p.'from' "
                "FROM pragma_foreign_key_list((SELECT name FROM sqlite_master WHERE type='table')) p "
                "WHERE NOT EXISTS ("
                "  SELECT 1 FROM pragma_index_list(p.'table') il, pragma_index_info(il.name) ii "
                "  WHERE ii.name = p.'from'"
                ")");
        } else {
            fprintf(stderr, "Usage: .lint fkey-indexes\n");
        }
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
    FILE *out = get_output(state);
    switch (state->mode) {
        case MODE_LIST:
            for (int i = 0; i < ncol; i++) {
                if (i > 0) fprintf(out, "%s", state->separator);
                fprintf(out, "%s", sqlite3_column_name(stmt, i));
            }
            fprintf(out, "\n");
            break;

        case MODE_CSV:
            for (int i = 0; i < ncol; i++) {
                if (i > 0) fprintf(out, ",");
                const char *name = sqlite3_column_name(stmt, i);
                /* Quote if contains comma, quote, or newline */
                if (strchr(name, ',') || strchr(name, '"') || strchr(name, '\n')) {
                    fprintf(out, "\"");
                    for (const char *p = name; *p; p++) {
                        if (*p == '"') fprintf(out, "\"\"");
                        else fputc(*p, out);
                    }
                    fprintf(out, "\"");
                } else {
                    fprintf(out, "%s", name);
                }
            }
            fprintf(out, "\n");
            break;

        case MODE_COLUMN:
        case MODE_TABLE:
        case MODE_MARKDOWN:
            for (int i = 0; i < ncol; i++) {
                int width = state->col_widths[i];
                if (width <= 0) width = 10;
                if (state->mode == MODE_TABLE || state->mode == MODE_MARKDOWN) {
                    fprintf(out, "| ");
                }
                fprintf(out, "%-*s", width, sqlite3_column_name(stmt, i));
                if (state->mode != MODE_TABLE && state->mode != MODE_MARKDOWN) {
                    fprintf(out, "  ");
                } else {
                    fprintf(out, " ");
                }
            }
            if (state->mode == MODE_TABLE || state->mode == MODE_MARKDOWN) {
                fprintf(out, "|");
            }
            fprintf(out, "\n");
            /* Print separator line */
            for (int i = 0; i < ncol; i++) {
                int width = state->col_widths[i];
                if (width <= 0) width = 10;
                if (state->mode == MODE_TABLE || state->mode == MODE_MARKDOWN) {
                    fprintf(out, "|");
                    for (int j = 0; j < width + 2; j++) fprintf(out, "-");
                } else {
                    for (int j = 0; j < width; j++) fprintf(out, "-");
                    fprintf(out, "  ");
                }
            }
            if (state->mode == MODE_TABLE || state->mode == MODE_MARKDOWN) {
                fprintf(out, "|");
            }
            fprintf(out, "\n");
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
    FILE *out = get_output(state);
    for (int i = 0; i < ncol; i++) {
        if (i > 0) fprintf(out, "%s", state->separator);
        if (sqlite3_column_type(stmt, i) == SQLITE_NULL) {
            fprintf(out, "%s", state->null_display);
        } else {
            fprintf(out, "%s", (const char *)sqlite3_column_text(stmt, i));
        }
    }
    fprintf(out, "\n");
}

/* Print a row in column mode */
static void print_row_column(CliState *state, sqlite3_stmt *stmt, int ncol) {
    FILE *out = get_output(state);
    for (int i = 0; i < ncol; i++) {
        int width = state->col_widths[i];
        if (width <= 0) width = 10;
        if (state->mode == MODE_TABLE || state->mode == MODE_MARKDOWN) {
            fprintf(out, "| ");
        }
        const char *val;
        if (sqlite3_column_type(stmt, i) == SQLITE_NULL) {
            val = state->null_display;
        } else {
            val = (const char *)sqlite3_column_text(stmt, i);
        }
        fprintf(out, "%-*.*s", width, width, val ? val : "");
        if (state->mode != MODE_TABLE && state->mode != MODE_MARKDOWN) {
            fprintf(out, "  ");
        } else {
            fprintf(out, " ");
        }
    }
    if (state->mode == MODE_TABLE || state->mode == MODE_MARKDOWN) {
        fprintf(out, "|");
    }
    fprintf(out, "\n");
}

/* Print a row in line mode */
static void print_row_line(CliState *state, sqlite3_stmt *stmt, int ncol) {
    FILE *out = get_output(state);
    for (int i = 0; i < ncol; i++) {
        const char *name = sqlite3_column_name(stmt, i);
        const char *val;
        if (sqlite3_column_type(stmt, i) == SQLITE_NULL) {
            val = state->null_display;
        } else {
            val = (const char *)sqlite3_column_text(stmt, i);
        }
        fprintf(out, "%12s = %s\n", name, val ? val : "");
    }
    fprintf(out, "\n");
}

/* Print a row in CSV mode */
static void print_row_csv(CliState *state, sqlite3_stmt *stmt, int ncol) {
    FILE *out = get_output(state);
    for (int i = 0; i < ncol; i++) {
        if (i > 0) fprintf(out, ",");
        if (sqlite3_column_type(stmt, i) == SQLITE_NULL) {
            /* Empty for NULL in CSV */
        } else {
            const char *val = (const char *)sqlite3_column_text(stmt, i);
            if (val && (strchr(val, ',') || strchr(val, '"') || strchr(val, '\n'))) {
                fprintf(out, "\"");
                for (const char *p = val; *p; p++) {
                    if (*p == '"') fprintf(out, "\"\"");
                    else fputc(*p, out);
                }
                fprintf(out, "\"");
            } else {
                fprintf(out, "%s", val ? val : "");
            }
        }
    }
    fprintf(out, "\n");
}

/* Execute SQL and display results */
static int execute_sql(CliState *state, const char *sql) {
    sqlite3_stmt *stmt = NULL;
    const char *tail = sql;
    int rc = SQLITE_OK;
    FILE *out = get_output(state);
    clock_t start_time = 0;

    if (!state->db) {
        fprintf(stderr, "Error: no database open\n");
        return 1;
    }

    /* Start timer if enabled */
    if (state->timer_on) {
        start_time = clock();
    }

    /* Trace SQL if enabled */
    if (state->trace_on) {
        fprintf(stderr, "TRACE: %s\n", sql);
    }

    while (tail && *tail) {
        /* Skip whitespace */
        while (*tail && isspace((unsigned char)*tail)) tail++;
        if (!*tail) break;

        /* Run EXPLAIN QUERY PLAN if enabled */
        if (state->eqp_on) {
            char eqp_sql[MAX_SQL_BUFFER];
            snprintf(eqp_sql, sizeof(eqp_sql), "EXPLAIN QUERY PLAN %s", tail);
            sqlite3_stmt *eqp_stmt = NULL;
            if (sqlite3_prepare_v2(state->db, eqp_sql, -1, &eqp_stmt, NULL) == SQLITE_OK) {
                fprintf(out, "QUERY PLAN\n");
                while (sqlite3_step(eqp_stmt) == SQLITE_ROW) {
                    int id = sqlite3_column_int(eqp_stmt, 0);
                    int parent = sqlite3_column_int(eqp_stmt, 1);
                    const char *detail = (const char *)sqlite3_column_text(eqp_stmt, 3);
                    fprintf(out, "|--%-*s%s\n", id * 3, "", detail ? detail : "");
                    (void)parent; /* unused but part of result */
                }
                sqlite3_finalize(eqp_stmt);
            }
        }

        rc = sqlite3_prepare_v2(state->db, tail, -1, &stmt, &tail);
        if (rc != SQLITE_OK) {
            fprintf(stderr, "Error: %s\n", sqlite3_errmsg(state->db));
            close_once(state);
            return 1;
        }

        if (!stmt) continue;  /* Empty statement */

        int ncol = sqlite3_column_count(stmt);
        int row_count = 0;
        bool headers_printed = false;

        if (state->mode == MODE_JSON && ncol > 0) {
            fprintf(out, "[");
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
                        if (row_count > 0) fprintf(out, ",");
                        fprintf(out, "{");
                        for (int i = 0; i < ncol; i++) {
                            if (i > 0) fprintf(out, ",");
                            fprintf(out, "\"%s\":", sqlite3_column_name(stmt, i));
                            int type = sqlite3_column_type(stmt, i);
                            if (type == SQLITE_NULL) {
                                fprintf(out, "null");
                            } else if (type == SQLITE_INTEGER) {
                                fprintf(out, "%lld", sqlite3_column_int64(stmt, i));
                            } else if (type == SQLITE_FLOAT) {
                                fprintf(out, "%g", sqlite3_column_double(stmt, i));
                            } else {
                                const char *val = (const char *)sqlite3_column_text(stmt, i);
                                fprintf(out, "\"");
                                for (const char *p = val; p && *p; p++) {
                                    switch (*p) {
                                        case '"': fprintf(out, "\\\""); break;
                                        case '\\': fprintf(out, "\\\\"); break;
                                        case '\n': fprintf(out, "\\n"); break;
                                        case '\r': fprintf(out, "\\r"); break;
                                        case '\t': fprintf(out, "\\t"); break;
                                        default: fputc(*p, out);
                                    }
                                }
                                fprintf(out, "\"");
                            }
                        }
                        fprintf(out, "}");
                        break;
                }
            }
            row_count++;
        }

        if (state->mode == MODE_JSON && ncol > 0) {
            fprintf(out, "]\n");
        }

        if (rc != SQLITE_DONE && rc != SQLITE_ROW) {
            fprintf(stderr, "Error: %s\n", sqlite3_errmsg(state->db));
            sqlite3_finalize(stmt);
            close_once(state);
            return 1;
        }

        state->changes = sqlite3_changes(state->db);

        if (state->stats_on) {
            fprintf(out, "Rows returned: %d\n", row_count);
            fprintf(out, "Changes: %d\n", state->changes);
        }

        sqlite3_finalize(stmt);
        stmt = NULL;
    }

    /* Print timer if enabled */
    if (state->timer_on) {
        clock_t end_time = clock();
        double elapsed = (double)(end_time - start_time) / CLOCKS_PER_SEC;
        fprintf(stderr, "Run Time: real %.3f\n", elapsed);
    }

    /* Close .once file if used */
    close_once(state);

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

/* Process SQL from a file */
static int process_file_input(CliState *state, const char *filename) {
    FILE *fp = fopen(filename, "r");
    if (!fp) {
        fprintf(stderr, "Error: cannot open \"%s\"\n", filename);
        return 1;
    }

    char line[MAX_LINE];
    char sql_buf[MAX_SQL_BUFFER] = "";
    int rc = 0;

    while (fgets(line, sizeof(line), fp)) {
        char *trimmed = trim(line);

        /* Skip empty lines and comments */
        if (trimmed[0] == '\0' || (trimmed[0] == '-' && trimmed[1] == '-')) {
            continue;
        }

        /* Handle dot commands */
        if (trimmed[0] == '.' && sql_buf[0] == '\0') {
            if (state->echo_on) {
                printf("%s\n", trimmed);
            }
            int meta_rc = do_meta_command(state, trimmed);
            if (meta_rc < 0) {
                /* Exit requested */
                break;
            }
            if (meta_rc != 0 && state->bail_on_error) {
                rc = meta_rc;
                break;
            }
            continue;
        }

        /* Accumulate SQL */
        size_t sql_len = strlen(sql_buf);
        size_t line_len = strlen(trimmed);
        if (sql_len + line_len + 2 < sizeof(sql_buf)) {
            if (sql_buf[0] != '\0') {
                strcat(sql_buf, " ");
            }
            strcat(sql_buf, trimmed);
        } else {
            fprintf(stderr, "Error: SQL too long in %s\n", filename);
            sql_buf[0] = '\0';
            continue;
        }

        /* Check if SQL is complete */
        if (sqlite3_complete(sql_buf)) {
            if (state->echo_on) {
                printf("%s\n", sql_buf);
            }
            int sql_rc = execute_sql(state, sql_buf);
            sql_buf[0] = '\0';
            if (sql_rc != 0 && state->bail_on_error) {
                rc = sql_rc;
                break;
            }
        }
    }

    /* Execute any remaining SQL */
    if (sql_buf[0] != '\0') {
        if (state->echo_on) {
            printf("%s\n", sql_buf);
        }
        int sql_rc = execute_sql(state, sql_buf);
        if (sql_rc != 0) {
            rc = sql_rc;
        }
    }

    fclose(fp);
    return rc;
}

/* Parse a CSV line into fields */
static int parse_csv_line(char *line, char **fields, int max_fields, char separator) {
    int n = 0;
    char *p = line;
    bool in_quotes = false;

    while (*p && n < max_fields) {
        fields[n] = p;

        /* Skip to end of field */
        while (*p) {
            if (*p == '"') {
                if (in_quotes && *(p + 1) == '"') {
                    /* Escaped quote */
                    memmove(p, p + 1, strlen(p));
                } else {
                    in_quotes = !in_quotes;
                    memmove(p, p + 1, strlen(p));
                    continue;
                }
            } else if (*p == separator && !in_quotes) {
                *p = '\0';
                p++;
                break;
            } else if (*p == '\n' || *p == '\r') {
                *p = '\0';
                break;
            }
            p++;
        }
        n++;
    }

    return n;
}

/* Import CSV file into a table */
static int import_csv(CliState *state, const char *filename, const char *table) {
    FILE *fp = fopen(filename, "r");
    if (!fp) {
        fprintf(stderr, "Error: cannot open \"%s\"\n", filename);
        return 1;
    }

    char line[MAX_LINE];
    char *fields[MAX_COLUMNS];
    int ncols = 0;
    int nrows = 0;
    char separator = ',';

    /* Detect separator: use tab if filename ends with .tsv */
    size_t len = strlen(filename);
    if (len > 4 && strcasecmp(filename + len - 4, ".tsv") == 0) {
        separator = '\t';
    }

    /* Read header line */
    if (!fgets(line, sizeof(line), fp)) {
        fprintf(stderr, "Error: empty file\n");
        fclose(fp);
        return 1;
    }

    ncols = parse_csv_line(line, fields, MAX_COLUMNS, separator);
    if (ncols == 0) {
        fprintf(stderr, "Error: no columns in header\n");
        fclose(fp);
        return 1;
    }

    /* Build CREATE TABLE and INSERT statements */
    char create_sql[MAX_SQL_BUFFER];
    char insert_sql[MAX_SQL_BUFFER];

    snprintf(create_sql, sizeof(create_sql), "CREATE TABLE IF NOT EXISTS \"%s\" (", table);
    snprintf(insert_sql, sizeof(insert_sql), "INSERT INTO \"%s\" VALUES (", table);

    for (int i = 0; i < ncols; i++) {
        if (i > 0) {
            strncat(create_sql, ", ", sizeof(create_sql) - strlen(create_sql) - 1);
            strncat(insert_sql, ", ", sizeof(insert_sql) - strlen(insert_sql) - 1);
        }
        /* Sanitize column name */
        char col_name[256];
        snprintf(col_name, sizeof(col_name), "\"%s\" TEXT", fields[i]);
        strncat(create_sql, col_name, sizeof(create_sql) - strlen(create_sql) - 1);
        strncat(insert_sql, "?", sizeof(insert_sql) - strlen(insert_sql) - 1);
    }
    strncat(create_sql, ")", sizeof(create_sql) - strlen(create_sql) - 1);
    strncat(insert_sql, ")", sizeof(insert_sql) - strlen(insert_sql) - 1);

    /* Create table */
    char *errmsg = NULL;
    int rc = sqlite3_exec(state->db, create_sql, NULL, NULL, &errmsg);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "Error creating table: %s\n", errmsg);
        sqlite3_free(errmsg);
        fclose(fp);
        return 1;
    }

    /* Prepare insert statement */
    sqlite3_stmt *stmt = NULL;
    rc = sqlite3_prepare_v2(state->db, insert_sql, -1, &stmt, NULL);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "Error preparing insert: %s\n", sqlite3_errmsg(state->db));
        fclose(fp);
        return 1;
    }

    /* Begin transaction for performance */
    sqlite3_exec(state->db, "BEGIN TRANSACTION", NULL, NULL, NULL);

    /* Read data lines */
    while (fgets(line, sizeof(line), fp)) {
        int n = parse_csv_line(line, fields, MAX_COLUMNS, separator);
        if (n == 0) continue;

        sqlite3_reset(stmt);
        for (int i = 0; i < ncols && i < n; i++) {
            sqlite3_bind_text(stmt, i + 1, fields[i], -1, SQLITE_TRANSIENT);
        }
        /* Bind NULL for missing columns */
        for (int i = n; i < ncols; i++) {
            sqlite3_bind_null(stmt, i + 1);
        }

        rc = sqlite3_step(stmt);
        if (rc != SQLITE_DONE) {
            fprintf(stderr, "Error inserting row: %s\n", sqlite3_errmsg(state->db));
        } else {
            nrows++;
        }
    }

    /* Commit transaction */
    sqlite3_exec(state->db, "COMMIT", NULL, NULL, NULL);

    sqlite3_finalize(stmt);
    fclose(fp);

    printf("Imported %d rows into table \"%s\"\n", nrows, table);
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
