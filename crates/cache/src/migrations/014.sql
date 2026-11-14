-- Rows keyed under the retired per-build scheme are never read: a state
-- namespace or a derived representation ending `+<build>`. `+<build>` sits
-- before the representation's closing quote; `+` elsewhere in a key is not
-- one.
DELETE FROM state WHERE instr(namespace, '+') > 0;
DELETE FROM http
WHERE instr(key, '"representation":"') > 0
  AND instr(substr(key, instr(key, '"representation":"') + 18), '+') > 0
  AND instr(substr(key, instr(key, '"representation":"') + 18), '+')
    < instr(substr(key, instr(key, '"representation":"') + 18), '"');
PRAGMA user_version = 14;
