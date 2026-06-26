//! sqlite-utils schema-shaped CLI commands ported to SQLink
//! dotcmd-aware world. 14 dot commands; see Cargo.toml.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use alloc::vec;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "dotcmd-aware",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::dot_command::{
        Guest as DotCommandGuest, InvokeContext, InvokeResult,
    };
    use bindings::exports::sqlite::extension::metadata::{
        DotCommandSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

    const FID_VIEWS:        u64 = 1;
    const FID_TRIGGERS:     u64 = 2;
    const FID_CREATE_TABLE: u64 = 3;
    const FID_CREATE_INDEX: u64 = 4;
    const FID_CREATE_VIEW:  u64 = 5;
    const FID_DROP_TABLE:   u64 = 6;
    const FID_DROP_VIEW:    u64 = 7;
    const FID_RENAME_TABLE: u64 = 8;
    const FID_DUPLICATE:    u64 = 9;
    const FID_ADD_COLUMN:   u64 = 10;
    const FID_TRANSFORM:    u64 = 11;
    const FID_EXTRACT:      u64 = 12;
    const FID_ADD_FK:       u64 = 13;
    const FID_ADD_FKS:      u64 = 14;
    const FID_INDEX_FKS:    u64 = 15;

    struct Ext;

    fn spec(id: u64, name: &str, summary: &str, usage: &str, help: &str) -> DotCommandSpec {
        DotCommandSpec {
            id,
            name: name.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            summary: summary.into(),
            usage: usage.into(),
            help: help.into(),
            examples: vec![],
            requires_write: false,
            no_args: false,
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "sqlite-utils-schema".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: vec![],
                aggregate_functions: vec![],
                collations: vec![],
                vtabs: vec![],
                dot_commands: vec![
                    spec(FID_VIEWS, "views",
                        "List views in the database",
                        "views",
                        "Print each view name + its first-line CREATE SQL."),
                    spec(FID_TRIGGERS, "triggers",
                        "List triggers in the database",
                        "triggers",
                        "Print each trigger as table.trigger."),
                    spec(FID_CREATE_TABLE, "create_table",
                        "Create a new table from a column spec",
                        "create_table NAME COL:TYPE [COL:TYPE ...] [--pk COL] [--not-null COL]",
                        "Types: int, text, real, blob, bool. --pk marks a primary key column."),
                    spec(FID_CREATE_INDEX, "create_index",
                        "Create an index on a table",
                        "create_index TABLE COL [COL ...] [--unique] [--if-not-exists] [--name N]",
                        "Default index name: idx_TABLE_COL1_COL2..."),
                    spec(FID_CREATE_VIEW, "create_view",
                        "Create a view",
                        "create_view NAME SELECT ...",
                        "Everything after NAME is treated as the view's SELECT statement."),
                    spec(FID_DROP_TABLE, "drop_table",
                        "Drop a table",
                        "drop_table NAME [--ignore]",
                        "--ignore adds IF EXISTS."),
                    spec(FID_DROP_VIEW, "drop_view",
                        "Drop a view",
                        "drop_view NAME [--ignore]",
                        "--ignore adds IF EXISTS."),
                    spec(FID_RENAME_TABLE, "rename_table",
                        "Rename a table",
                        "rename_table OLD NEW",
                        "ALTER TABLE OLD RENAME TO NEW."),
                    spec(FID_DUPLICATE, "duplicate",
                        "Duplicate a table (rows only, no PK/indexes)",
                        "duplicate OLD NEW",
                        "CREATE TABLE NEW AS SELECT * FROM OLD."),
                    spec(FID_ADD_COLUMN, "add_column",
                        "Add a column to a table",
                        "add_column TABLE COL TYPE",
                        "ALTER TABLE TABLE ADD COLUMN COL TYPE."),
                    spec(FID_TRANSFORM, "transform",
                        "Rewrite a table (rename / drop / retype / reorder cols)",
                        "transform TABLE [--rename OLD NEW] [--drop COL] [--type COL TYPE] [--pk COL] [--column-order COL,COL,...]",
                        "Creates a new table, copies rows, swaps. All in one transaction."),
                    spec(FID_EXTRACT, "extract",
                        "Extract columns into a lookup table",
                        "extract TABLE COL [COL ...] [--table LOOKUP] [--fk-col FK]",
                        "Pulls the named cols into LOOKUP, replaces them with an FK in TABLE."),
                    spec(FID_ADD_FK, "add_fk",
                        "Add a foreign key constraint",
                        "add_fk TABLE COL OTHER_TABLE [OTHER_COL]",
                        "Uses the transform path; OTHER_COL defaults to id."),
                    spec(FID_ADD_FKS, "add_fks",
                        "Add multiple foreign key constraints in one tx",
                        "add_fks TABLE COL OTHER [OTHER_COL] [TABLE COL OTHER [OTHER_COL] ...]",
                        "Repeats .add_fk for each 3- or 4-tuple of args."),
                    spec(FID_INDEX_FKS, "index_fks",
                        "Auto-create indexes for every foreign key column",
                        "index_fks",
                        "Walks PRAGMA foreign_key_list for every table; creates idx_T_C for missing FK indexes."),
                ],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                declared_capabilities: vec![],
                optional_capabilities: vec![],
                preferred_prefix: Some("sqlite_schema".into()),
                prefix_expansion: Some("org.sqlite.utils.schema".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("sqlite-utils-schema: no scalar functions".into())
        }
    }

    impl DotCommandGuest for Ext {
        fn invoke(func_id: u64, ctx: InvokeContext) -> Result<InvokeResult, SqliteError> {
            let args = ctx.args.trim();
            Ok(match func_id {
                FID_VIEWS        => cmd_views(),
                FID_TRIGGERS     => cmd_triggers(),
                FID_CREATE_TABLE => cmd_create_table(args),
                FID_CREATE_INDEX => cmd_create_index(args),
                FID_CREATE_VIEW  => cmd_create_view(args),
                FID_DROP_TABLE   => cmd_drop_x(args, "TABLE"),
                FID_DROP_VIEW    => cmd_drop_x(args, "VIEW"),
                FID_RENAME_TABLE => cmd_rename_table(args),
                FID_DUPLICATE    => cmd_duplicate(args),
                FID_ADD_COLUMN   => cmd_add_column(args),
                FID_TRANSFORM    => cmd_transform(args),
                FID_EXTRACT      => cmd_extract(args),
                FID_ADD_FK       => cmd_add_fk(args),
                FID_ADD_FKS      => cmd_add_fks(args),
                FID_INDEX_FKS    => cmd_index_fks(),
                other => err(format!("sqlite-utils-schema: unknown func id {other}")),
            })
        }
    }

    // -------- Helpers --------

    fn text(body: String) -> InvokeResult {
        InvokeResult { text: body, state_deltas: vec![], ok: true, exit_code: 0 }
    }

    fn err(message: String) -> InvokeResult {
        InvokeResult {
            text: format!("Error: {message}\n"),
            state_deltas: vec![],
            ok: false,
            exit_code: 1,
        }
    }

    fn tok(s: &str) -> Vec<String> {
        s.split_whitespace().map(|w| w.to_string()).collect()
    }

    /// Map shorthand type names to SQLite affinity tokens.
    fn type_token(t: &str) -> &'static str {
        match t.to_lowercase().as_str() {
            "int" | "integer" | "bool" | "boolean" => "INTEGER",
            "text" | "str" | "string" => "TEXT",
            "real" | "float" | "double" => "REAL",
            "blob" | "bytes" => "BLOB",
            _ => "TEXT",
        }
    }

    fn quote_ident(s: &str) -> String {
        format!("\"{}\"", s.replace('"', "\"\""))
    }

    fn quote_lit(s: &str) -> String {
        format!("'{}'", s.replace('\'', "''"))
    }

    /// Pull column-name / column-type from `PRAGMA table_info(TABLE)`.
    /// Returns Vec<(name, type, notnull, dflt_value_sql, pk_index)>.
    struct ColInfo {
        name: String,
        ty: String,
        notnull: bool,
        dflt: Option<String>,
        pk: i64,
    }

    fn table_info(table: &str) -> Result<Vec<ColInfo>, String> {
        let sql = format!("PRAGMA table_info({})", quote_ident(table));
        let r = spi::execute(&sql, &[]).map_err(|e| e.message)?;
        let mut out = Vec::with_capacity(r.rows.len());
        for row in r.rows {
            // cid, name, type, notnull, dflt_value, pk
            let name = match row.get(1) {
                Some(SqlValue::Text(s)) => s.clone(),
                _ => continue,
            };
            let ty = match row.get(2) {
                Some(SqlValue::Text(s)) => s.clone(),
                _ => String::new(),
            };
            let notnull = matches!(row.get(3), Some(SqlValue::Integer(n)) if *n != 0);
            let dflt = match row.get(4) {
                Some(SqlValue::Text(s)) => Some(s.clone()),
                Some(SqlValue::Integer(n)) => Some(n.to_string()),
                Some(SqlValue::Real(r))    => Some(r.to_string()),
                _ => None,
            };
            let pk = match row.get(5) {
                Some(SqlValue::Integer(n)) => *n,
                _ => 0,
            };
            out.push(ColInfo { name, ty, notnull, dflt, pk });
        }
        if out.is_empty() {
            return Err(format!("no such table: {table}"));
        }
        Ok(out)
    }

    fn list_tables() -> Result<Vec<String>, String> {
        let r = spi::execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            &[],
        ).map_err(|e| e.message)?;
        Ok(r.rows.into_iter().filter_map(|row| match row.into_iter().next() {
            Some(SqlValue::Text(s)) => Some(s),
            _ => None,
        }).collect())
    }

    // -------- views / triggers --------

    fn cmd_views() -> InvokeResult {
        let r = match spi::execute(
            "SELECT name, COALESCE(sql,'') FROM sqlite_master WHERE type='view' ORDER BY name",
            &[],
        ) {
            Ok(r) => r,
            Err(e) => return err(format!(".views: {}", e.message)),
        };
        if r.rows.is_empty() {
            return text("(no views)\n".into());
        }
        let mut out = String::new();
        for row in r.rows {
            let name = match row.first() { Some(SqlValue::Text(s)) => s.clone(), _ => continue };
            let sql = match row.get(1) { Some(SqlValue::Text(s)) => s.clone(), _ => String::new() };
            let first_line = sql.lines().next().unwrap_or("").trim();
            if first_line.is_empty() {
                out.push_str(&format!("{name}\n"));
            } else {
                out.push_str(&format!("{name}  {first_line}\n"));
            }
        }
        text(out)
    }

    fn cmd_triggers() -> InvokeResult {
        let r = match spi::execute(
            "SELECT name, tbl_name FROM sqlite_master WHERE type='trigger' ORDER BY tbl_name, name",
            &[],
        ) {
            Ok(r) => r,
            Err(e) => return err(format!(".triggers: {}", e.message)),
        };
        if r.rows.is_empty() {
            return text("(no triggers)\n".into());
        }
        let mut out = String::new();
        for row in r.rows {
            let name = match row.first() { Some(SqlValue::Text(s)) => s.clone(), _ => continue };
            let tbl  = match row.get(1)   { Some(SqlValue::Text(s)) => s.clone(), _ => String::new() };
            out.push_str(&format!("{tbl}.{name}\n"));
        }
        text(out)
    }

    // -------- create_table --------

    fn cmd_create_table(args: &str) -> InvokeResult {
        let mut toks = tok(args);
        if toks.is_empty() {
            return err(".create_table NAME COL:TYPE [COL:TYPE ...] [--pk COL]".into());
        }
        let name = toks.remove(0);
        let mut pk: Option<String> = None;
        let mut not_null: Vec<String> = vec![];
        let mut cols: Vec<(String, String)> = vec![];
        let mut i = 0;
        while i < toks.len() {
            let t = toks[i].clone();
            if t == "--pk" {
                if i + 1 >= toks.len() { return err("--pk requires a column name".into()); }
                pk = Some(toks[i + 1].clone());
                i += 2;
            } else if t == "--not-null" {
                if i + 1 >= toks.len() { return err("--not-null requires a column name".into()); }
                not_null.push(toks[i + 1].clone());
                i += 2;
            } else if let Some((col, ty)) = t.split_once(':') {
                cols.push((col.to_string(), type_token(ty).to_string()));
                i += 1;
            } else {
                return err(format!("expected COL:TYPE, got {t:?}"));
            }
        }
        if cols.is_empty() {
            return err("at least one COL:TYPE required".into());
        }
        let mut parts: Vec<String> = vec![];
        for (col, ty) in &cols {
            let mut p = format!("{} {ty}", quote_ident(col));
            if pk.as_deref() == Some(col.as_str()) {
                p.push_str(" PRIMARY KEY");
            }
            if not_null.iter().any(|n| n == col) {
                p.push_str(" NOT NULL");
            }
            parts.push(p);
        }
        let sql = format!("CREATE TABLE {} ({})", quote_ident(&name), parts.join(", "));
        match spi::execute_batch(&sql) {
            Ok(_) => text(format!("Created table {name} with {} columns\n", cols.len())),
            Err(e) => err(format!(".create_table {name}: {}", e.message)),
        }
    }

    // -------- create_index --------

    fn cmd_create_index(args: &str) -> InvokeResult {
        let toks = tok(args);
        if toks.is_empty() {
            return err(".create_index TABLE COL [COL ...] [--unique] [--if-not-exists] [--name N]".into());
        }
        let mut table: Option<String> = None;
        let mut cols: Vec<String> = vec![];
        let mut unique = false;
        let mut ifne = false;
        let mut name: Option<String> = None;
        let mut i = 0;
        while i < toks.len() {
            let t = &toks[i];
            if t == "--unique" { unique = true; i += 1; }
            else if t == "--if-not-exists" { ifne = true; i += 1; }
            else if t == "--name" {
                if i + 1 >= toks.len() { return err("--name requires a value".into()); }
                name = Some(toks[i + 1].clone()); i += 2;
            } else if table.is_none() {
                table = Some(t.clone()); i += 1;
            } else {
                cols.push(t.clone()); i += 1;
            }
        }
        let table = match table { Some(t) => t, None => return err("missing TABLE".into()) };
        if cols.is_empty() { return err("at least one column required".into()); }
        let idx_name = name.unwrap_or_else(|| format!("idx_{}_{}", table, cols.join("_")));
        let unique_s = if unique { "UNIQUE " } else { "" };
        let ifne_s   = if ifne { "IF NOT EXISTS " } else { "" };
        let col_list = cols.iter().map(|c| quote_ident(c)).collect::<Vec<_>>().join(", ");
        let sql = format!(
            "CREATE {unique_s}INDEX {ifne_s}{} ON {} ({col_list})",
            quote_ident(&idx_name), quote_ident(&table)
        );
        match spi::execute_batch(&sql) {
            Ok(_) => text(format!("Created index {idx_name} on {table}({})\n", cols.join(", "))),
            Err(e) => err(format!(".create_index: {}", e.message)),
        }
    }

    // -------- create_view --------

    fn cmd_create_view(args: &str) -> InvokeResult {
        let (name, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
        let name = name.trim();
        let body = rest.trim();
        if name.is_empty() || body.is_empty() {
            return err(".create_view NAME SELECT ...".into());
        }
        let sql = format!("CREATE VIEW {} AS {body}", quote_ident(name));
        match spi::execute_batch(&sql) {
            Ok(_) => text(format!("Created view {name}\n")),
            Err(e) => err(format!(".create_view {name}: {}", e.message)),
        }
    }

    // -------- drop_table / drop_view --------

    fn cmd_drop_x(args: &str, kind: &str) -> InvokeResult {
        let toks = tok(args);
        if toks.is_empty() { return err(format!(".drop_{} NAME [--ignore]", kind.to_lowercase())); }
        let mut ignore = false;
        let mut name: Option<String> = None;
        for t in toks {
            if t == "--ignore" { ignore = true; }
            else if name.is_none() { name = Some(t); }
        }
        let name = name.unwrap();
        let ifexists = if ignore { "IF EXISTS " } else { "" };
        let sql = format!("DROP {} {ifexists}{}", kind, quote_ident(&name));
        match spi::execute_batch(&sql) {
            Ok(_) => text(format!("Dropped {} {name}\n", kind.to_lowercase())),
            Err(e) => err(format!(".drop_{} {name}: {}", kind.to_lowercase(), e.message)),
        }
    }

    // -------- rename_table --------

    fn cmd_rename_table(args: &str) -> InvokeResult {
        let toks = tok(args);
        if toks.len() != 2 { return err(".rename_table OLD NEW".into()); }
        let sql = format!(
            "ALTER TABLE {} RENAME TO {}",
            quote_ident(&toks[0]), quote_ident(&toks[1])
        );
        match spi::execute_batch(&sql) {
            Ok(_) => text(format!("Renamed {} -> {}\n", toks[0], toks[1])),
            Err(e) => err(format!(".rename_table: {}", e.message)),
        }
    }

    // -------- duplicate --------

    fn cmd_duplicate(args: &str) -> InvokeResult {
        let toks = tok(args);
        if toks.len() != 2 { return err(".duplicate OLD NEW".into()); }
        let sql = format!(
            "CREATE TABLE {} AS SELECT * FROM {}",
            quote_ident(&toks[1]), quote_ident(&toks[0])
        );
        match spi::execute_batch(&sql) {
            Ok(_) => text(format!("Duplicated {} -> {} (rows only; no PK/indexes)\n", toks[0], toks[1])),
            Err(e) => err(format!(".duplicate: {}", e.message)),
        }
    }

    // -------- add_column --------

    fn cmd_add_column(args: &str) -> InvokeResult {
        let toks = tok(args);
        if toks.len() != 3 { return err(".add_column TABLE COL TYPE".into()); }
        let sql = format!(
            "ALTER TABLE {} ADD COLUMN {} {}",
            quote_ident(&toks[0]), quote_ident(&toks[1]), type_token(&toks[2])
        );
        match spi::execute_batch(&sql) {
            Ok(_) => text(format!("Added column {}.{} {}\n", toks[0], toks[1], type_token(&toks[2]))),
            Err(e) => err(format!(".add_column: {}", e.message)),
        }
    }

    // -------- transform --------
    //
    // Builds a new table per the rename/drop/retype/reorder/pk
    // requests, copies rows over, drops the original, renames the
    // new one. All in one tx.

    #[derive(Clone)]
    struct TransformSpec {
        renames: Vec<(String, String)>, // (old, new)
        drops:   Vec<String>,
        types:   Vec<(String, String)>, // (col, type)
        pk:      Option<String>,
        order:   Option<Vec<String>>,
        // For .add_fk / .add_fks  fold the FK clauses into the new schema.
        fks:     Vec<(String, String, String)>, // (col, other_table, other_col)
    }

    impl TransformSpec {
        fn new() -> Self {
            Self { renames: vec![], drops: vec![], types: vec![], pk: None, order: None, fks: vec![] }
        }
    }

    fn parse_transform_args(toks: &[String]) -> Result<(String, TransformSpec), String> {
        if toks.is_empty() { return Err(".transform TABLE [...]".into()); }
        let table = toks[0].clone();
        let mut sp = TransformSpec::new();
        let mut i = 1;
        while i < toks.len() {
            let t = &toks[i];
            match t.as_str() {
                "--rename" => {
                    if i + 2 >= toks.len() { return Err("--rename OLD NEW".into()); }
                    sp.renames.push((toks[i + 1].clone(), toks[i + 2].clone()));
                    i += 3;
                }
                "--drop" => {
                    if i + 1 >= toks.len() { return Err("--drop COL".into()); }
                    sp.drops.push(toks[i + 1].clone());
                    i += 2;
                }
                "--type" => {
                    if i + 2 >= toks.len() { return Err("--type COL TYPE".into()); }
                    sp.types.push((toks[i + 1].clone(), type_token(&toks[i + 2]).to_string()));
                    i += 3;
                }
                "--pk" => {
                    if i + 1 >= toks.len() { return Err("--pk COL".into()); }
                    sp.pk = Some(toks[i + 1].clone());
                    i += 2;
                }
                "--column-order" => {
                    if i + 1 >= toks.len() { return Err("--column-order COL,COL,...".into()); }
                    sp.order = Some(toks[i + 1].split(',').map(|s| s.trim().to_string()).collect());
                    i += 2;
                }
                other => return Err(format!("unknown flag {other:?}")),
            }
        }
        Ok((table, sp))
    }

    /// The actual transform engine. Returns the resulting (NEW name
    /// after rename, count of rows copied). Runs inside an existing
    /// transaction — caller wraps BEGIN/COMMIT.
    fn run_transform(table: &str, sp: &TransformSpec) -> Result<i64, String> {
        let info = table_info(table)?;
        // Build column lineage: (source_name, target_name, type).
        // 1. start from current cols
        // 2. apply renames
        // 3. apply retypes
        // 4. drop dropped
        // 5. reorder if --column-order set
        let mut cols: Vec<(String, String, String)> = info.iter()
            .map(|c| {
                let mut new_name = c.name.clone();
                for (old, new) in &sp.renames {
                    if &c.name == old { new_name = new.clone(); break; }
                }
                let ty = sp.types.iter()
                    .find(|(t_col, _)| {
                        // type targets match against NEW name (post-rename)
                        t_col == &new_name || t_col == &c.name
                    })
                    .map(|(_, t)| t.clone())
                    .unwrap_or_else(|| c.ty.clone());
                (c.name.clone(), new_name, ty)
            })
            .filter(|(src, _, _)| !sp.drops.contains(src))
            .collect();
        if let Some(order) = &sp.order {
            let mut reordered: Vec<(String, String, String)> = vec![];
            for want in order {
                if let Some(pos) = cols.iter().position(|(_, n, _)| n == want) {
                    reordered.push(cols.remove(pos));
                }
            }
            reordered.extend(cols);
            cols = reordered;
        }
        if cols.is_empty() {
            return Err("transform: no columns remaining after --drop".into());
        }

        // Build new-table schema.
        let mut col_defs: Vec<String> = vec![];
        for (_src, tgt, ty) in &cols {
            let mut def = format!("{} {ty}", quote_ident(tgt));
            if sp.pk.as_deref() == Some(tgt.as_str()) {
                def.push_str(" PRIMARY KEY");
            }
            col_defs.push(def);
        }
        for (col, other_t, other_c) in &sp.fks {
            col_defs.push(format!(
                "FOREIGN KEY({}) REFERENCES {}({})",
                quote_ident(col), quote_ident(other_t), quote_ident(other_c)
            ));
        }

        // Use a deterministic temp name; we're inside one tx so collisions are unlikely.
        let new_table = format!("{table}_sqlink_transform");
        let create_sql = format!(
            "CREATE TABLE {} ({})",
            quote_ident(&new_table), col_defs.join(", ")
        );
        spi::execute_batch(&create_sql).map_err(|e| format!("create new table: {}", e.message))?;

        let tgt_list = cols.iter().map(|(_, n, _)| quote_ident(n)).collect::<Vec<_>>().join(", ");
        let src_list = cols.iter().map(|(s, _, _)| quote_ident(s)).collect::<Vec<_>>().join(", ");
        let copy_sql = format!(
            "INSERT INTO {} ({tgt_list}) SELECT {src_list} FROM {}",
            quote_ident(&new_table), quote_ident(table)
        );
        let copied = spi::execute_batch(&copy_sql).map_err(|e| format!("copy rows: {}", e.message))?;

        let drop_sql = format!("DROP TABLE {}", quote_ident(table));
        spi::execute_batch(&drop_sql).map_err(|e| format!("drop old: {}", e.message))?;

        let rename_sql = format!("ALTER TABLE {} RENAME TO {}",
            quote_ident(&new_table), quote_ident(table));
        spi::execute_batch(&rename_sql).map_err(|e| format!("rename new: {}", e.message))?;

        Ok(copied)
    }

    fn cmd_transform(args: &str) -> InvokeResult {
        let toks = tok(args);
        let (table, sp) = match parse_transform_args(&toks) {
            Ok(v) => v,
            Err(e) => return err(format!(".transform: {e}")),
        };
        if let Err(e) = spi::execute_batch("BEGIN") {
            return err(format!(".transform: BEGIN: {}", e.message));
        }
        match run_transform(&table, &sp) {
            Ok(n) => {
                if let Err(e) = spi::execute_batch("COMMIT") {
                    let _ = spi::execute_batch("ROLLBACK");
                    return err(format!(".transform: COMMIT: {}", e.message));
                }
                text(format!("Transformed {table}: {n} rows copied\n"))
            }
            Err(msg) => {
                let _ = spi::execute_batch("ROLLBACK");
                err(format!(".transform {table}: {msg}"))
            }
        }
    }

    // -------- extract --------

    fn cmd_extract(args: &str) -> InvokeResult {
        let toks = tok(args);
        if toks.len() < 2 {
            return err(".extract TABLE COL [COL ...] [--table LOOKUP] [--fk-col FK]".into());
        }
        let table = toks[0].clone();
        let mut cols: Vec<String> = vec![];
        let mut lookup_name: Option<String> = None;
        let mut fk_col: Option<String> = None;
        let mut i = 1;
        while i < toks.len() {
            match toks[i].as_str() {
                "--table" => {
                    if i + 1 >= toks.len() { return err("--table requires a name".into()); }
                    lookup_name = Some(toks[i + 1].clone()); i += 2;
                }
                "--fk-col" => {
                    if i + 1 >= toks.len() { return err("--fk-col requires a name".into()); }
                    fk_col = Some(toks[i + 1].clone()); i += 2;
                }
                other if !other.starts_with("--") => {
                    cols.push(other.to_string()); i += 1;
                }
                other => return err(format!("unknown flag {other:?}")),
            }
        }
        if cols.is_empty() { return err("at least one COL required".into()); }
        let lookup = lookup_name.unwrap_or_else(|| format!("{}_lookup", cols.join("_")));
        let fk = fk_col.unwrap_or_else(|| format!("{lookup}_id"));

        // Pull table_info to learn the cols' types.
        let info = match table_info(&table) {
            Ok(v) => v,
            Err(e) => return err(format!(".extract: {e}")),
        };
        let mut col_types: Vec<(String, String)> = vec![];
        for c in &cols {
            match info.iter().find(|ci| &ci.name == c) {
                Some(ci) => col_types.push((c.clone(), ci.ty.clone())),
                None => return err(format!("extract: no column {c} in {table}")),
            }
        }

        if let Err(e) = spi::execute_batch("BEGIN") {
            return err(format!(".extract: BEGIN: {}", e.message));
        }

        // 1. Create lookup (id PK, cols, UNIQUE).
        let mut lookup_cols = vec!["id INTEGER PRIMARY KEY".to_string()];
        for (c, ty) in &col_types {
            lookup_cols.push(format!("{} {}", quote_ident(c), ty));
        }
        let uniq = col_types.iter().map(|(c, _)| quote_ident(c)).collect::<Vec<_>>().join(", ");
        lookup_cols.push(format!("UNIQUE ({uniq})"));
        let create_sql = format!("CREATE TABLE IF NOT EXISTS {} ({})", quote_ident(&lookup), lookup_cols.join(", "));
        if let Err(e) = spi::execute_batch(&create_sql) {
            let _ = spi::execute_batch("ROLLBACK");
            return err(format!(".extract: create lookup: {}", e.message));
        }
        // 2. Populate lookup.
        let col_list = col_types.iter().map(|(c, _)| quote_ident(c)).collect::<Vec<_>>().join(", ");
        let pop_sql = format!(
            "INSERT OR IGNORE INTO {} ({col_list}) SELECT DISTINCT {col_list} FROM {}",
            quote_ident(&lookup), quote_ident(&table)
        );
        if let Err(e) = spi::execute_batch(&pop_sql) {
            let _ = spi::execute_batch("ROLLBACK");
            return err(format!(".extract: populate lookup: {}", e.message));
        }
        // 3. Add FK column to source.
        let add_fk_sql = format!(
            "ALTER TABLE {} ADD COLUMN {} INTEGER REFERENCES {}({})",
            quote_ident(&table), quote_ident(&fk), quote_ident(&lookup), quote_ident("id")
        );
        if let Err(e) = spi::execute_batch(&add_fk_sql) {
            let _ = spi::execute_batch("ROLLBACK");
            return err(format!(".extract: add fk col: {}", e.message));
        }
        // 4. Populate FK.
        let join_pred = col_types.iter()
            .map(|(c, _)| format!(
                "{lt}.{c} = {st}.{c}",
                lt = quote_ident(&lookup),
                st = quote_ident(&table),
                c = quote_ident(c)
            ))
            .collect::<Vec<_>>().join(" AND ");
        let upd_sql = format!(
            "UPDATE {st} SET {fk} = (SELECT id FROM {lt} WHERE {join_pred})",
            st = quote_ident(&table),
            fk = quote_ident(&fk),
            lt = quote_ident(&lookup),
        );
        if let Err(e) = spi::execute_batch(&upd_sql) {
            let _ = spi::execute_batch("ROLLBACK");
            return err(format!(".extract: populate fk: {}", e.message));
        }
        // 5. Drop the extracted cols via transform.
        let mut sp = TransformSpec::new();
        sp.drops = cols.clone();
        if let Err(msg) = run_transform(&table, &sp) {
            let _ = spi::execute_batch("ROLLBACK");
            return err(format!(".extract: drop extracted cols: {msg}"));
        }
        if let Err(e) = spi::execute_batch("COMMIT") {
            let _ = spi::execute_batch("ROLLBACK");
            return err(format!(".extract: COMMIT: {}", e.message));
        }
        text(format!("Extracted {cols:?} from {table} into {lookup} (fk: {fk})\n"))
    }

    // -------- add_fk / add_fks --------

    fn cmd_add_fk(args: &str) -> InvokeResult {
        let toks = tok(args);
        if toks.len() < 3 {
            return err(".add_fk TABLE COL OTHER_TABLE [OTHER_COL]".into());
        }
        let table = toks[0].clone();
        let col   = toks[1].clone();
        let other = toks[2].clone();
        let other_col = toks.get(3).cloned().unwrap_or_else(|| "id".to_string());

        if let Err(e) = spi::execute_batch("BEGIN") {
            return err(format!(".add_fk: BEGIN: {}", e.message));
        }
        let mut sp = TransformSpec::new();
        sp.fks.push((col.clone(), other.clone(), other_col.clone()));
        match run_transform(&table, &sp) {
            Ok(_) => {
                if let Err(e) = spi::execute_batch("COMMIT") {
                    let _ = spi::execute_batch("ROLLBACK");
                    return err(format!(".add_fk: COMMIT: {}", e.message));
                }
                text(format!("Added FK {table}.{col} -> {other}({other_col})\n"))
            }
            Err(msg) => {
                let _ = spi::execute_batch("ROLLBACK");
                err(format!(".add_fk {table}: {msg}"))
            }
        }
    }

    fn cmd_add_fks(args: &str) -> InvokeResult {
        // Naive: parse fk triples greedily; user can supply 3- or 4-tuples.
        // For simplicity we require 4-tuples (TABLE COL OTHER OTHER_COL).
        // If the user wants the default OTHER_COL=id they can repeat .add_fk.
        let toks = tok(args);
        if toks.len() < 4 || toks.len() % 4 != 0 {
            return err(".add_fks TABLE COL OTHER OTHER_COL [TABLE COL OTHER OTHER_COL ...]".into());
        }
        if let Err(e) = spi::execute_batch("BEGIN") {
            return err(format!(".add_fks: BEGIN: {}", e.message));
        }
        // Group fks by table so we run a single transform per table.
        let mut by_table: Vec<(String, Vec<(String, String, String)>)> = vec![];
        for chunk in toks.chunks(4) {
            let t = chunk[0].clone();
            let entry = (chunk[1].clone(), chunk[2].clone(), chunk[3].clone());
            if let Some(slot) = by_table.iter_mut().find(|(tn, _)| tn == &t) {
                slot.1.push(entry);
            } else {
                by_table.push((t, vec![entry]));
            }
        }
        let mut report: Vec<String> = vec![];
        for (table, fks) in &by_table {
            let mut sp = TransformSpec::new();
            sp.fks = fks.clone();
            if let Err(msg) = run_transform(table, &sp) {
                let _ = spi::execute_batch("ROLLBACK");
                return err(format!(".add_fks {table}: {msg}"));
            }
            for (col, other, other_col) in fks {
                report.push(format!("  {table}.{col} -> {other}({other_col})"));
            }
        }
        if let Err(e) = spi::execute_batch("COMMIT") {
            let _ = spi::execute_batch("ROLLBACK");
            return err(format!(".add_fks: COMMIT: {}", e.message));
        }
        text(format!("Added {} FK(s):\n{}\n", report.len(), report.join("\n")))
    }

    // -------- index_fks --------

    fn cmd_index_fks() -> InvokeResult {
        let tables = match list_tables() {
            Ok(v) => v,
            Err(e) => return err(format!(".index_fks: {e}")),
        };
        let mut created: Vec<String> = vec![];
        let mut already: Vec<String> = vec![];
        for t in &tables {
            // PRAGMA foreign_key_list: id, seq, table, from, to, on_update, on_delete, match
            let fk_sql = format!("PRAGMA foreign_key_list({})", quote_ident(t));
            let r = match spi::execute(&fk_sql, &[]) {
                Ok(r) => r,
                Err(e) => return err(format!(".index_fks {t}: {}", e.message)),
            };
            for row in r.rows {
                let col = match row.get(3) {
                    Some(SqlValue::Text(s)) => s.clone(),
                    _ => continue,
                };
                // Check if any index already covers this single column.
                let idx_sql = format!("PRAGMA index_list({})", quote_ident(t));
                let idx_r = match spi::execute(&idx_sql, &[]) {
                    Ok(r) => r,
                    Err(e) => return err(format!(".index_fks {t}: {}", e.message)),
                };
                let mut covered = false;
                for irow in idx_r.rows {
                    let iname = match irow.get(1) { Some(SqlValue::Text(s)) => s.clone(), _ => continue };
                    let info_sql = format!("PRAGMA index_info({})", quote_ident(&iname));
                    let info_r = match spi::execute(&info_sql, &[]) {
                        Ok(r) => r,
                        Err(_) => continue,
                    };
                    let cols: Vec<String> = info_r.rows.iter().filter_map(|r| match r.get(2) {
                        Some(SqlValue::Text(s)) => Some(s.clone()),
                        _ => None,
                    }).collect();
                    if cols.len() == 1 && cols[0] == col {
                        covered = true;
                        break;
                    }
                }
                if covered {
                    already.push(format!("{t}.{col}"));
                    continue;
                }
                let idx_name = format!("idx_{t}_{col}");
                let create_sql = format!(
                    "CREATE INDEX IF NOT EXISTS {} ON {} ({})",
                    quote_ident(&idx_name), quote_ident(t), quote_ident(&col)
                );
                if let Err(e) = spi::execute_batch(&create_sql) {
                    return err(format!(".index_fks: create {idx_name}: {}", e.message));
                }
                created.push(format!("{t}.{col}"));
            }
        }
        let mut out = String::new();
        out.push_str(&format!("Created {} index(es)", created.len()));
        if !created.is_empty() {
            out.push_str(": ");
            out.push_str(&created.join(", "));
        }
        out.push('\n');
        if !already.is_empty() {
            out.push_str(&format!("(already covered: {})\n", already.join(", ")));
        }
        text(out)
    }

    bindings::export!(Ext with_types_in bindings);
}
