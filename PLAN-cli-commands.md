# Plan: Add Missing CLI Commands for SQLite WASM

## Overview

This plan adds missing CLI dot commands to the SQLite WASM CLI that make sense for a WebAssembly environment. Commands requiring OS shell access, external applications, or low-level system features are excluded.

## Current Status

### Already Implemented (26 commands)
- `.bail on|off` - Stop after error
- `.databases` - List databases
- `.dump ?TABLE?` - Dump database as SQL
- `.echo on|off` - Echo commands
- `.exit` / `.quit` - Exit program
- `.extensions` - List WASM extensions (custom)
- `.headers on|off` - Show headers
- `.help` - Show help
- `.indexes ?TABLE?` - Show indexes
- `.load FILE` - Load WASM extension (custom)
- `.mode MODE` - Set output mode
- `.nullvalue STRING` - NULL display value
- `.open ?FILE?` - Open database
- `.print STRING...` - Print string
- `.prompt MAIN CONT` - Set prompts
- `.schema ?TABLE?` - Show schema
- `.separator STRING` - Set separator
- `.show` - Show settings
- `.stats on|off` - Show statistics
- `.tables ?PATTERN?` - List tables
- `.unload NAME` - Unload extension (custom)
- `.version` - Show version
- `.width NUM...` - Set column widths

### Listed in Help but NOT Implemented (2 commands)
- `.output ?FILE?` - Redirect output (stub only)
- `.read FILE` - Execute SQL from file (stub only)

## Commands to Add

### Priority 1: Critical Missing Commands (Fix Broken Help)

#### 1. `.read FILE` - Execute SQL from File
**File:** `src/cli/sqlite_cli.c`
```c
else if (strcasecmp(cmd_name, "read") == 0) {
    if (!arg1) {
        fprintf(stderr, "Usage: .read FILENAME\n");
    } else {
        FILE *fp = fopen(arg1, "r");
        if (!fp) {
            fprintf(stderr, "Error: cannot open \"%s\"\n", arg1);
        } else {
            char line[MAX_LINE];
            char sql_buf[MAX_SQL_BUFFER] = "";
            while (fgets(line, sizeof(line), fp)) {
                char *trimmed = trim(line);
                if (trimmed[0] == '.') {
                    do_meta_command(state, trimmed);
                } else if (trimmed[0] != '\0') {
                    strncat(sql_buf, trimmed, sizeof(sql_buf) - strlen(sql_buf) - 2);
                    strcat(sql_buf, " ");
                    if (sqlite3_complete(sql_buf)) {
                        execute_sql(state, sql_buf);
                        sql_buf[0] = '\0';
                    }
                }
            }
            if (sql_buf[0] != '\0') {
                execute_sql(state, sql_buf);
            }
            fclose(fp);
        }
    }
}
```

#### 2. `.output ?FILE?` - Redirect Output
**File:** `src/cli/sqlite_cli.c`

Add to CliState:
```c
FILE *output_file;
char output_filename[512];
```

Implementation:
```c
else if (strcasecmp(cmd_name, "output") == 0) {
    if (!arg1 || strcmp(arg1, "stdout") == 0) {
        if (state->output_file && state->output_file != stdout) {
            fclose(state->output_file);
        }
        state->output_file = stdout;
        state->output_filename[0] = '\0';
        printf("Output redirected to stdout\n");
    } else {
        FILE *fp = fopen(arg1, "w");
        if (!fp) {
            fprintf(stderr, "Error: cannot open \"%s\"\n", arg1);
        } else {
            if (state->output_file && state->output_file != stdout) {
                fclose(state->output_file);
            }
            state->output_file = fp;
            strncpy(state->output_filename, arg1, sizeof(state->output_filename) - 1);
        }
    }
}
```

### Priority 2: Data Management Commands

#### 3. `.import FILE TABLE` - Import CSV/TSV Data
```c
else if (strcasecmp(cmd_name, "import") == 0) {
    if (!arg1 || !arg2) {
        fprintf(stderr, "Usage: .import FILE TABLE\n");
    } else {
        // Implementation: parse CSV/TSV and INSERT into table
        import_csv(state, arg1, arg2);
    }
}
```

#### 4. `.backup ?DB? FILE` - Backup Database
```c
else if (strcasecmp(cmd_name, "backup") == 0) {
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
        if (sqlite3_open(filename, &dest) == SQLITE_OK) {
            sqlite3_backup *backup = sqlite3_backup_init(dest, "main", state->db, db_name);
            if (backup) {
                sqlite3_backup_step(backup, -1);
                sqlite3_backup_finish(backup);
            }
            sqlite3_close(dest);
            printf("Database backed up to %s\n", filename);
        }
    }
}
```

#### 5. `.restore ?DB? FILE` - Restore Database
```c
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
        if (sqlite3_open(filename, &src) == SQLITE_OK) {
            sqlite3_backup *backup = sqlite3_backup_init(state->db, db_name, src, "main");
            if (backup) {
                sqlite3_backup_step(backup, -1);
                sqlite3_backup_finish(backup);
            }
            sqlite3_close(src);
            printf("Database restored from %s\n", filename);
        }
    }
}
```

#### 6. `.clone NEWDB` - Clone Database
```c
else if (strcasecmp(cmd_name, "clone") == 0) {
    if (!arg1) {
        fprintf(stderr, "Usage: .clone NEWDB\n");
    } else {
        // Clone is equivalent to backup to new file
        // Reuse backup implementation
    }
}
```

#### 7. `.save FILE` - Save Database (alias for .backup)
```c
else if (strcasecmp(cmd_name, "save") == 0) {
    // Alias for .backup main FILE
}
```

### Priority 3: Query Analysis Commands

#### 8. `.changes` - Show Row Changes
```c
else if (strcasecmp(cmd_name, "changes") == 0) {
    printf("Changes: %d\n", sqlite3_changes(state->db));
    printf("Total changes: %d\n", sqlite3_total_changes(state->db));
}
```

#### 9. `.timer on|off` - SQL Execution Timer
Add to CliState:
```c
bool timer_on;
```

Wrap execute_sql with timing:
```c
else if (strcasecmp(cmd_name, "timer") == 0) {
    if (arg1) {
        state->timer_on = parse_on_off(arg1);
    } else {
        printf("timer is %s\n", state->timer_on ? "on" : "off");
    }
}
```

#### 10. `.timeout MS` - Set Busy Timeout
```c
else if (strcasecmp(cmd_name, "timeout") == 0) {
    if (arg1) {
        int ms = atoi(arg1);
        sqlite3_busy_timeout(state->db, ms);
        printf("Timeout set to %d ms\n", ms);
    } else {
        fprintf(stderr, "Usage: .timeout MILLISECONDS\n");
    }
}
```

#### 11. `.trace on|off|FILE` - Trace SQL Statements
```c
else if (strcasecmp(cmd_name, "trace") == 0) {
    if (!arg1 || strcasecmp(arg1, "off") == 0) {
        sqlite3_trace_v2(state->db, 0, NULL, NULL);
        state->trace_on = false;
    } else {
        sqlite3_trace_v2(state->db, SQLITE_TRACE_STMT, trace_callback, state);
        state->trace_on = true;
    }
}
```

#### 12. `.explain on|off|auto` - EXPLAIN Mode
```c
else if (strcasecmp(cmd_name, "explain") == 0) {
    if (!arg1) {
        printf("explain is %s\n", state->explain_mode);
    } else if (strcasecmp(arg1, "on") == 0) {
        state->explain_mode = EXPLAIN_ON;
    } else if (strcasecmp(arg1, "off") == 0) {
        state->explain_mode = EXPLAIN_OFF;
    } else if (strcasecmp(arg1, "auto") == 0) {
        state->explain_mode = EXPLAIN_AUTO;
    }
}
```

#### 13. `.eqp on|off|full|trigger` - EXPLAIN QUERY PLAN
```c
else if (strcasecmp(cmd_name, "eqp") == 0) {
    if (!arg1) {
        printf("eqp is %s\n", state->eqp_on ? "on" : "off");
    } else {
        state->eqp_on = parse_on_off(arg1);
    }
}
```

### Priority 4: Database Information Commands

#### 14. `.fullschema` - Full Schema with Stats
```c
else if (strcasecmp(cmd_name, "fullschema") == 0) {
    execute_sql(state,
        "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL "
        "UNION ALL "
        "SELECT 'ANALYZE sqlite_master;'");
    execute_sql(state,
        "SELECT sql FROM sqlite_stat1 WHERE sql IS NOT NULL");
}
```

#### 15. `.dbinfo ?DB?` - Database Information
```c
else if (strcasecmp(cmd_name, "dbinfo") == 0) {
    const char *db = arg1 ? arg1 : "main";
    printf("database: %s\n", state->db_filename);
    execute_sql(state, "SELECT * FROM pragma_database_list");
    execute_sql(state, "SELECT * FROM pragma_page_count");
    execute_sql(state, "SELECT * FROM pragma_page_size");
    execute_sql(state, "SELECT * FROM pragma_freelist_count");
    execute_sql(state, "SELECT * FROM pragma_encoding");
    execute_sql(state, "SELECT * FROM pragma_journal_mode");
}
```

#### 16. `.dbconfig ?op? ?val?` - Database Config
```c
else if (strcasecmp(cmd_name, "dbconfig") == 0) {
    // List or change sqlite3_db_config() options
    // Common options: defensive, writable_schema, etc.
}
```

#### 17. `.limit ?LIMIT? ?VAL?` - Display/Change Limits
```c
else if (strcasecmp(cmd_name, "limit") == 0) {
    if (!arg1) {
        // Print all limits
        printf("SQLITE_LIMIT_LENGTH: %d\n", sqlite3_limit(state->db, SQLITE_LIMIT_LENGTH, -1));
        printf("SQLITE_LIMIT_SQL_LENGTH: %d\n", sqlite3_limit(state->db, SQLITE_LIMIT_SQL_LENGTH, -1));
        // ... etc
    } else if (arg2) {
        // Set specific limit
    }
}
```

### Priority 5: Output Commands

#### 18. `.once FILE` - Output Next Command to File
```c
else if (strcasecmp(cmd_name, "once") == 0) {
    if (!arg1) {
        fprintf(stderr, "Usage: .once FILENAME\n");
    } else {
        state->once_file = fopen(arg1, "w");
        if (!state->once_file) {
            fprintf(stderr, "Error: cannot open \"%s\"\n", arg1);
        }
    }
}
```

### Priority 6: Integrity & Analysis Commands

#### 19. `.sha3sum ?TABLE?` - Compute SHA3 Hash
```c
else if (strcasecmp(cmd_name, "sha3sum") == 0) {
    // Requires SHA3 implementation or extension
    // Can use built-in if compiled with SQLITE_ENABLE_SHA3
}
```

#### 20. `.lint fkey-indexes` - Report Schema Issues
```c
else if (strcasecmp(cmd_name, "lint") == 0) {
    if (arg1 && strcasecmp(arg1, "fkey-indexes") == 0) {
        // Check for missing indexes on foreign key columns
        execute_sql(state,
            "SELECT 'Missing index for FK: ' || fk.'table' || '.' || fk.'from' "
            "FROM pragma_foreign_key_list AS fk "
            "WHERE NOT EXISTS (SELECT 1 FROM pragma_index_list(fk.'table') AS il "
            "JOIN pragma_index_info(il.name) AS ii ON ii.name = fk.'from')");
    }
}
```

### Priority 7: VFS Information Commands

#### 21. `.vfslist` - List VFSes
```c
else if (strcasecmp(cmd_name, "vfslist") == 0) {
    sqlite3_vfs *vfs = sqlite3_vfs_find(NULL);
    printf("Available VFSes:\n");
    while (vfs) {
        printf("  %s%s\n", vfs->zName,
               vfs == sqlite3_vfs_find(NULL) ? " (default)" : "");
        vfs = vfs->pNext;
    }
}
```

#### 22. `.vfsname` - Current VFS Name
```c
else if (strcasecmp(cmd_name, "vfsname") == 0) {
    sqlite3_vfs *vfs = sqlite3_vfs_find(NULL);
    if (vfs) {
        printf("%s\n", vfs->zName);
    }
}
```

### Priority 8: Parameter Commands

#### 23. `.parameter CMD VALUE` - SQL Parameters
```c
else if (strcasecmp(cmd_name, "parameter") == 0) {
    if (!arg1) {
        fprintf(stderr, "Usage: .parameter init|list|set NAME VALUE|clear|unset NAME\n");
    } else if (strcasecmp(arg1, "list") == 0) {
        // List bound parameters
    } else if (strcasecmp(arg1, "set") == 0) {
        // Set parameter
    } else if (strcasecmp(arg1, "clear") == 0) {
        // Clear all parameters
    }
}
```

## Commands NOT to Implement (WASM Incompatible)

| Command | Reason |
|---------|--------|
| `.shell CMD` | Requires OS shell access |
| `.system CMD` | Requires OS shell access |
| `.cd DIR` | Not practical in WASM sandbox |
| `.excel` | Requires external application |
| `.www` | Requires browser launch from CLI |
| `.archive` | Requires libzip/miniz |
| `.expert` | Requires expert extension |
| `.recover` | Complex corruption recovery |
| `.session` | Requires session extension |
| `.filectrl` | Low-level file control |
| `.crlf` | Windows-specific line endings |
| `.imposter` | Advanced internal feature |
| `.intck` | Requires intck extension |
| `.nonce` | Security feature for safe mode |
| `.check`/`.testcase` | Internal testing only |
| `.dbtotxt` | Debug hex dump feature |
| `.unmodule` | Virtual table module management |
| `.auth` | Authorizer callbacks (advanced) |
| `.scanstats` | Requires SQLITE_ENABLE_STMT_SCANSTATUS |
| `.progress` | Low-level progress callback |
| `.connection` | Multi-connection management (complex) |

## Implementation Order

### Phase 1: Fix Broken Commands
1. `.read FILE` - Already in help, needs implementation
2. `.output FILE` - Already in help, needs implementation

### Phase 2: Essential Data Commands
3. `.import FILE TABLE` - CSV import
4. `.backup FILE` - Database backup
5. `.restore FILE` - Database restore
6. `.clone NEWDB` - Clone database
7. `.save FILE` - Alias for backup

### Phase 3: Query Tools
8. `.changes` - Row change count
9. `.timer on|off` - Execution timing
10. `.timeout MS` - Busy timeout
11. `.trace on|off` - SQL tracing
12. `.explain on|off` - EXPLAIN mode
13. `.eqp on|off` - EXPLAIN QUERY PLAN

### Phase 4: Information Commands
14. `.fullschema` - Complete schema
15. `.dbinfo` - Database info
16. `.dbconfig` - Config options
17. `.limit` - SQLite limits

### Phase 5: Advanced Features
18. `.once FILE` - Single output redirect
19. `.lint` - Schema analysis
20. `.vfslist` - List VFSes
21. `.vfsname` - VFS name
22. `.parameter` - SQL parameters

## Files to Modify

1. **`src/cli/sqlite_cli.c`**
   - Add new state fields (output_file, timer_on, trace_on, etc.)
   - Add implementations in `do_meta_command()`
   - Update `print_help()` with new commands
   - Add helper functions (import_csv, trace_callback, etc.)

2. **`tests/cli/test_commands.sh`** (new file)
   - Add tests for new commands

## Testing Plan

```bash
# Test .read
echo "SELECT 1;" > /tmp/test.sql
wasmtime run build/sqlite-cli.wasm -- :memory: ".read /tmp/test.sql"

# Test .output
wasmtime run build/sqlite-cli.wasm -- :memory: ".output /tmp/out.txt" "SELECT 1;"

# Test .backup/.restore
wasmtime run --dir=. build/sqlite-cli.wasm -- test.db "CREATE TABLE t(x);" ".backup backup.db"
wasmtime run --dir=. build/sqlite-cli.wasm -- new.db ".restore backup.db" "SELECT * FROM t;"

# Test .import
echo -e "a,b\n1,2\n3,4" > /tmp/data.csv
wasmtime run --dir=. build/sqlite-cli.wasm -- :memory: ".import /tmp/data.csv test" "SELECT * FROM test;"

# Test .timer
wasmtime run build/sqlite-cli.wasm -- :memory: ".timer on" "SELECT * FROM sqlite_master;"

# Test .changes
wasmtime run build/sqlite-cli.wasm -- :memory: "CREATE TABLE t(x);" "INSERT INTO t VALUES(1),(2),(3);" ".changes"
```

## Estimated Effort

| Phase | Commands | Complexity | Priority |
|-------|----------|------------|----------|
| Phase 1 | 2 | Low | High |
| Phase 2 | 5 | Medium | High |
| Phase 3 | 6 | Low-Medium | Medium |
| Phase 4 | 4 | Low | Medium |
| Phase 5 | 5 | Medium | Low |

Total: 22 new commands

## References

- [SQLite CLI Documentation](https://sqlite.org/cli.html)
- [SQLite C API](https://sqlite.org/c3ref/intro.html)
