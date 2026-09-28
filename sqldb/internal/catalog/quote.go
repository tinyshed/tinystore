package catalog

import "strings"

// Quote is a name as SQL reads it: a keyword, or anything but letters, digits
// and underscores not starting with a digit, goes in double quotes
//
//	notes → notes    order → "order"    created at → "created at"
func Quote(name string) string {
	if !keywords[strings.ToUpper(name)] && plainName(name) {
		return name
	}
	return `"` + strings.ReplaceAll(name, `"`, `""`) + `"`
}

func plainName(name string) bool {
	for i, r := range name {
		letter := r == '_' || (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z')
		if !letter && (i == 0 || r < '0' || r > '9') {
			return false
		}
	}
	return name != ""
}

// keywords are SQLite's, which a name must be quoted to be
var keywords = func() map[string]bool {
	words := map[string]bool{}
	for word := range strings.FieldsSeq(`ABORT ACTION ADD AFTER ALL ALTER ALWAYS ANALYZE AND AS ASC ATTACH
		AUTOINCREMENT BEFORE BEGIN BETWEEN BY CASCADE CASE CAST CHECK COLLATE COLUMN COMMIT CONFLICT CONSTRAINT
		CREATE CROSS CURRENT CURRENT_DATE CURRENT_TIME CURRENT_TIMESTAMP DATABASE DEFAULT DEFERRABLE DEFERRED
		DELETE DESC DETACH DISTINCT DO DROP EACH ELSE END ESCAPE EXCEPT EXCLUDE EXCLUSIVE EXISTS EXPLAIN FAIL
		FILTER FIRST FOLLOWING FOR FOREIGN FROM FULL GENERATED GLOB GROUP GROUPS HAVING IF IGNORE IMMEDIATE IN
		INDEX INDEXED INITIALLY INNER INSERT INSTEAD INTERSECT INTO IS ISNULL JOIN KEY LAST LEFT LIKE LIMIT MATCH
		MATERIALIZED NATURAL NO NOT NOTHING NOTNULL NULL NULLS OF OFFSET ON OR ORDER OTHERS OUTER OVER PARTITION
		PLAN PRAGMA PRECEDING PRIMARY QUERY RAISE RANGE RECURSIVE REFERENCES REGEXP REINDEX RELEASE RENAME
		REPLACE RESTRICT RETURNING RIGHT ROLLBACK ROW ROWS SAVEPOINT SELECT SET TABLE TEMP TEMPORARY THEN TIES
		TO TRANSACTION TRIGGER UNBOUNDED UNION UNIQUE UPDATE USING VACUUM VALUES VIEW VIRTUAL WHEN WHERE
		WINDOW WITH WITHOUT`) {
		words[word] = true
	}
	return words
}()
