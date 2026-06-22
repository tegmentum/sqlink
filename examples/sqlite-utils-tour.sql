.create_table dogs id:int name:text breed:text age:int --pk id
.add_column dogs adopted bool
.create_index dogs name
.create_view young_dogs SELECT * FROM dogs WHERE age < 3
.views
.triggers
.insert dogs /tmp/sqlite-utils-tour-dogs.json --pk id
.rows dogs
.analyze_tables dogs
.upsert dogs /tmp/sqlite-utils-tour-dogs.json --pk id
.convert dogs name "upper(name)"
.rows dogs
.duplicate dogs dogs_bak
.rename_table dogs_bak dogs_backup
.enable_fts dogs name breed --create-triggers
.search dogs CLEO
.search dogs corgi
.rebuild_fts dogs
.enable_wal
.enable_counts
SELECT "table", count FROM _counts WHERE "table" = 'dogs' OR "table" = 'dogs_backup' ORDER BY "table";
.analyze
.optimize
.reset_counts
.disable_wal
.create_database
.disable_fts dogs
.drop_view young_dogs
.drop_table dogs_backup
.help
