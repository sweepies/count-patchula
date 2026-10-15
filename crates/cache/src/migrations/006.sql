-- Keep causes by generation so an in-flight batch cannot consume follow-up IDs.
CREATE TABLE server_work_causes (
    installation INTEGER NOT NULL,
    repo TEXT NOT NULL,
    generation INTEGER NOT NULL,
    attribution TEXT NOT NULL
);
CREATE INDEX server_work_causes_key ON server_work_causes(installation, repo, generation);
PRAGMA user_version = 6;
