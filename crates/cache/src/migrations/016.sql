-- The compressed HTTP body bytes the budget counts, kept by triggers in the
-- transaction of every write to `http`, so eviction compares one row to the
-- budget instead of summing the table, and every process sharing the file
-- sees the same total.
CREATE TABLE http_bytes (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  total INTEGER NOT NULL
);
INSERT INTO http_bytes(id, total) SELECT 1, COALESCE(SUM(length(body)), 0) FROM http;
CREATE TRIGGER http_bytes_insert AFTER INSERT ON http BEGIN
  UPDATE http_bytes SET total = total + length(NEW.body);
END;
CREATE TRIGGER http_bytes_delete AFTER DELETE ON http BEGIN
  UPDATE http_bytes SET total = total - length(OLD.body);
END;
CREATE TRIGGER http_bytes_update AFTER UPDATE OF body ON http BEGIN
  UPDATE http_bytes SET total = total - length(OLD.body) + length(NEW.body);
END;
PRAGMA user_version = 16;
